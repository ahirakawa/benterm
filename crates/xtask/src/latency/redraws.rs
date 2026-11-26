//! `cargo xtask redraws`: counts inside Benterm what the output of
//! terminals behind the window costs it. Benterm is started with
//! `BENTERM_REDRAW_STATS`, which makes it write its counts to a file.
//! The counts are taken for a stretch with no output, and for as long a
//! stretch while agents started behind the window with `agent run`
//! write lines.
//!
//! Everything goes through the socket and the file, so no permission to
//! record the screen or to control the computer is needed.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::ops::Sub;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::flood::read_progress;
use super::run::{RunDirs, app_command};
use super::{App, LatencyOptions};

/// What `cargo xtask redraws` was asked to count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedrawOptions {
    /// How long each stretch is counted.
    pub seconds: u32,
    /// The agents that write lines behind the window.
    pub panes: u32,
    /// The lines a second each of them writes.
    pub rate: u32,
    /// Where the results go, instead of a new directory under the target
    /// directory.
    pub out: Option<PathBuf>,
}

impl Default for RedrawOptions {
    fn default() -> Self {
        Self {
            seconds: 30,
            panes: 10,
            rate: 100,
            out: None,
        }
    }
}

/// What Benterm counted, as of a moment, or over a stretch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Since Benterm started counting, or the length of the stretch.
    pub elapsed: Duration,
    pub frames: u64,
    pub visible_redraws: u64,
    pub hidden_redraws: u64,
    pub wake_locks: u64,
    pub wake_lock_wait: Duration,
}

impl Sub for Counts {
    type Output = Counts;

    /// The counts of the stretch from `rhs` to `self`.
    fn sub(self, rhs: Counts) -> Counts {
        Counts {
            elapsed: self.elapsed.saturating_sub(rhs.elapsed),
            frames: self.frames.saturating_sub(rhs.frames),
            visible_redraws: self.visible_redraws.saturating_sub(rhs.visible_redraws),
            hidden_redraws: self.hidden_redraws.saturating_sub(rhs.hidden_redraws),
            wake_locks: self.wake_locks.saturating_sub(rhs.wake_locks),
            wake_lock_wait: self.wake_lock_wait.saturating_sub(rhs.wake_lock_wait),
        }
    }
}

/// Reads the file Benterm writes: a name and a number on each line.
pub fn parse_counts(text: &str) -> Result<Counts> {
    let mut counts = Counts::default();
    for line in text.lines() {
        let (name, value) = line
            .split_once(' ')
            .with_context(|| format!("not NAME VALUE: {line:?}"))?;
        let value: u64 = value
            .parse()
            .with_context(|| format!("not a number: {line:?}"))?;
        match name {
            "elapsed_ms" => counts.elapsed = Duration::from_millis(value),
            "frames" => counts.frames = value,
            "visible_redraws" => counts.visible_redraws = value,
            "hidden_redraws" => counts.hidden_redraws = value,
            "wake_locks" => counts.wake_locks = value,
            "wake_lock_wait_us" => counts.wake_lock_wait = Duration::from_micros(value),
            _ => bail!("Benterm wrote a count that is not understood: {line:?}"),
        }
    }
    Ok(counts)
}

/// A Markdown table of the two stretches side by side.
pub fn redraw_table(quiet: &Counts, busy: &Counts, panes: u32) -> String {
    let mut table = format!(
        "| Counted | No output | {panes} terminals behind the window writing |\n|---|---|---|\n"
    );
    let mut row = |name: &str, cell: &dyn Fn(&Counts) -> String| {
        writeln!(table, "| {name} | {} | {} |", cell(quiet), cell(busy))
            .expect("writing to a string does not fail");
    };
    let with_rate = |count: u64, counts: &Counts| {
        let seconds = counts.elapsed.as_secs_f64();
        if seconds > 0.0 {
            format!("{count} ({:.1}/s)", count as f64 / seconds)
        } else {
            format!("{count} (-)")
        }
    };
    let per_wake = |value: f64, counts: &Counts| {
        let wakes = counts.visible_redraws + counts.hidden_redraws;
        if wakes > 0 {
            format!("{:.1}", value / wakes as f64)
        } else {
            "-".to_string()
        }
    };
    row("Seconds", &|c| format!("{:.1}", c.elapsed.as_secs_f64()));
    row("Frames drawn", &|c| with_rate(c.frames, c));
    row("Redraws asked by terminals on view", &|c| {
        with_rate(c.visible_redraws, c)
    });
    row("Redraws asked by terminals not on view", &|c| {
        with_rate(c.hidden_redraws, c)
    });
    row("State locks taken on wakes", &|c| {
        with_rate(c.wake_locks, c)
    });
    row("Locks per wake", &|c| per_wake(c.wake_locks as f64, c));
    row("Lock wait per wake, µs", &|c| {
        per_wake(c.wake_lock_wait.as_secs_f64() * 1e6, c)
    });
    row("Lock wait in all, ms", &|c| {
        format!("{:.1}", c.wake_lock_wait.as_secs_f64() * 1e3)
    });
    table
}

/// The arguments of the `benterm` command that starts writer `pane`
/// as an agent named `name`. Started from outside any pane it has no
/// parent, so it runs behind the window.
pub fn agent_run_arguments(
    name: &str,
    writer: &Path,
    rate: u32,
    pane: u32,
    started: &Path,
) -> Vec<String> {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    vec![
        "agent".to_string(),
        "run".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--".to_string(),
        text(writer),
        "flood".to_string(),
        "--rate".to_string(),
        rate.to_string(),
        "--pane".to_string(),
        pane.to_string(),
        "--started-dir".to_string(),
        text(started),
    ]
}

/// How long Benterm has to open its socket and write its first counts,
/// and the writers to start.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the counting waits after each change before it starts a
/// stretch, so that the change itself is not in it.
const SETTLE: Duration = Duration::from_secs(1);

/// Waits until `ready` gives a value, for at most [`START_TIMEOUT`].
fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(value) = ready()? {
            return Ok(value);
        }
        if Instant::now() > deadline {
            bail!("gave up waiting for {what}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// A running Benterm, which is ended when this is dropped.
struct Benterm {
    child: Child,
    socket: PathBuf,
    counts: PathBuf,
}

impl Benterm {
    fn start(binary: &Path, dirs: &RunDirs, counts: PathBuf) -> Result<Self> {
        let app = app_command(App::Benterm, dirs, binary, Path::new(""));
        let mut command = Command::new(&app.command[0]);
        command
            .args(&app.command[1..])
            .envs(app.environment.iter().map(|(k, v)| (k, v)))
            .env("BENTERM_REDRAW_STATS", &counts)
            .stdin(Stdio::null());
        let child = command.spawn().context("start Benterm")?;
        let socket = dirs
            .home
            .join("state/benterm")
            .join(format!("{}.sock", child.id()));
        Ok(Self {
            child,
            socket,
            counts,
        })
    }

    /// The counts as of the last time Benterm wrote them, which it does a
    /// few times a second.
    fn read(&self) -> Result<Counts> {
        let text = fs::read_to_string(&self.counts)
            .with_context(|| format!("read {}", self.counts.display()))?;
        parse_counts(&text)
    }

    /// The counts of `seconds` from now on.
    fn count(&self, seconds: u32) -> Result<Counts> {
        thread::sleep(SETTLE);
        let start = self.read()?;
        thread::sleep(Duration::from_secs(u64::from(seconds)));
        Ok(self.read()? - start)
    }
}

impl Drop for Benterm {
    fn drop(&mut self) {
        // The writers end when their terminals go with the app.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A directory that is removed when the run is over. The path is short
/// because Benterm's socket is below it.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs `cargo xtask redraws` for the workspace at `workspace`, whose
/// build output is in `target`.
pub fn run(options: &RedrawOptions, workspace: &Path, target: &Path) -> Result<()> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "-p", "benterm"])
        .current_dir(workspace)
        .status()
        .context("run cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }
    let binary = target.join("release/benterm");
    let writer = env::current_exe().context("find this program")?;

    let scratch = Scratch(PathBuf::from(format!(
        "/tmp/benterm-redraws-{}",
        std::process::id()
    )));
    fs::create_dir(&scratch.0).with_context(|| format!("create {}", scratch.0.display()))?;
    let dirs = RunDirs::in_dir(&scratch.0);
    let latency = LatencyOptions {
        rate: options.rate,
        ..LatencyOptions::default()
    };
    dirs.write(&latency, &writer)?;

    eprintln!("starting Benterm");
    let benterm = Benterm::start(&binary, &dirs, scratch.0.join("redraws"))?;
    wait_for("Benterm's socket and its first counts", || {
        Ok((benterm.socket.exists() && benterm.counts.exists()).then_some(()))
    })?;

    eprintln!("counting {} seconds with no output", options.seconds);
    let quiet = benterm.count(options.seconds)?;

    eprintln!(
        "starting {} agents behind the window, each writing {} lines a second",
        options.panes, options.rate
    );
    for pane in 1..=options.panes {
        let arguments = agent_run_arguments(
            &format!("redraws-{pane}"),
            &writer,
            options.rate,
            pane,
            &dirs.started,
        );
        let output = Command::new(&binary)
            .args(&arguments)
            .env("BENTERM_SOCKET", &benterm.socket)
            .env_remove("BENTERM_PANE_ID")
            .output()
            .context("run benterm agent run")?;
        if !output.status.success() {
            bail!(
                "benterm agent run failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    wait_for("the writers to start", || {
        let (writers, _) = read_progress(&dirs.started)?;
        Ok((writers >= options.panes as usize).then_some(()))
    })?;

    eprintln!(
        "counting {} seconds with the agents writing",
        options.seconds
    );
    let busy = benterm.count(options.seconds)?;
    let (_, progress) = read_progress(&dirs.started)?;
    drop(benterm);

    let out = match &options.out {
        Some(out) => out.clone(),
        None => target.join("latency").join(timestamp()),
    };
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    let table = redraw_table(&quiet, &busy, options.panes);
    let document = format!(
        "# Benterm: redraws caused by terminals behind the window\n\n\
         - Benterm: the release binary started without a bundle, with BENTERM_REDRAW_STATS\n\
         - Writers: {} agents started with `agent run` from outside the window, {} lines a \
         second each; {} lines written and {} skipped in all\n\
         - Each stretch starts {} s after the change before it\n\n{table}\n\
         See the section on `cargo xtask redraws` in README.md for how to read the table.\n",
        options.panes,
        options.rate,
        progress.written,
        progress.skipped,
        SETTLE.as_secs(),
    );
    let results = out.join("redraws.md");
    fs::write(&results, document).with_context(|| format!("write {}", results.display()))?;
    println!("{table}");
    println!("{}", results.display());
    Ok(())
}

/// Now, as the directories of `cargo xtask latency` are named.
fn timestamp() -> String {
    Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

//! The whole measurement: build what is measured, start each app the
//! same way, let the probe time it, and write the results.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::flood::read_progress;
use super::report::{Measurement, ProbeOutput, Scenario, csv, table};
use super::setup::{
    MARKER, MARKER_CELLS, benterm_config, ghostty_binary, ghostty_config, ghostty_copy, plan_keys,
    split_plan, zshrc,
};
use super::{App, LatencyOptions, probe};

/// The files of one start of an app. Every start has its own, so that
/// nothing an earlier start saved is restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDirs {
    /// Benterm's home directory: its session and its socket are below it,
    /// and the user's own are left alone.
    pub home: PathBuf,
    pub benterm_config: PathBuf,
    pub ghostty_config: PathBuf,
    /// The directory zsh reads its `.zshrc` from.
    pub zdotdir: PathBuf,
    /// Where the writers say that they run.
    pub started: PathBuf,
}

impl RunDirs {
    pub fn in_dir(run: &Path) -> Self {
        let home = run.join("home");
        Self {
            benterm_config: home.join(".config/benterm/config.toml"),
            home,
            ghostty_config: run.join("ghostty.conf"),
            zdotdir: run.join("zsh"),
            started: run.join("started"),
        }
    }

    /// Makes the directories and writes the files both apps and their
    /// shells read.
    pub(crate) fn write(&self, options: &LatencyOptions, writer: &Path) -> Result<()> {
        for dir in [&self.home, &self.zdotdir, &self.started] {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let config_dir = self
            .benterm_config
            .parent()
            .expect("the config is in a directory");
        fs::create_dir_all(config_dir)?;
        fs::write(
            &self.benterm_config,
            benterm_config(&options.font, options.font_size),
        )?;
        fs::write(
            &self.ghostty_config,
            ghostty_config(&options.font, options.font_size),
        )?;
        fs::write(
            self.zdotdir.join(".zshrc"),
            zshrc(writer, options.rate, &self.started),
        )?;
        Ok(())
    }
}

/// How an app is started: its command and what is added to the
/// environment it would have had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppCommand {
    pub command: Vec<String>,
    pub environment: Vec<(String, String)>,
}

/// The command that starts `app` with the files of `dirs`. Both apps run
/// zsh as a login shell with the same `.zshrc`.
pub fn app_command(app: App, dirs: &RunDirs, benterm: &Path, ghostty: &Path) -> AppCommand {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let mut environment = vec![
        ("SHELL".to_string(), "/bin/zsh".to_string()),
        ("ZDOTDIR".to_string(), text(&dirs.zdotdir)),
    ];
    let command = match app {
        App::Benterm => {
            environment.extend([
                ("HOME".to_string(), text(&dirs.home)),
                (
                    "XDG_CONFIG_HOME".to_string(),
                    text(&dirs.home.join(".config")),
                ),
                ("XDG_STATE_HOME".to_string(), text(&dirs.home.join("state"))),
            ]);
            vec![text(benterm)]
        }
        // Without its default files Ghostty reads only this config, not
        // the user's.
        App::Ghostty => vec![
            text(ghostty),
            "--config-default-files=false".to_string(),
            format!("--config-file={}", dirs.ghostty_config.display()),
        ],
    };
    AppCommand {
        command,
        environment,
    }
}

/// What the probe is asked to time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// One start of the app, until its prompt shows.
    Start,
    /// `count` keys at the prompt of the only pane.
    Keys { count: u32 },
    /// `count` keys at the prompt of the first pane while `panes` more
    /// panes write lines.
    KeysUnderLoad { count: u32, panes: u32 },
}

pub fn probe_arguments(probe: &Probe, app: &AppCommand, dirs: &RunDirs) -> Vec<String> {
    let mut arguments = vec![
        match probe {
            Probe::Start => "start",
            Probe::Keys { .. } | Probe::KeysUnderLoad { .. } => "keys",
        }
        .to_string(),
    ];
    for (name, value) in &app.environment {
        arguments.extend(["--env".to_string(), format!("{name}={value}")]);
    }
    let [red, green, blue] = MARKER;
    arguments.extend([
        "--marker".to_string(),
        format!("{red:02x}{green:02x}{blue:02x}"),
        "--marker-cells".to_string(),
        MARKER_CELLS.to_string(),
    ]);
    match probe {
        Probe::Start => {}
        Probe::Keys { count } => arguments.extend(["--count".to_string(), count.to_string()]),
        Probe::KeysUnderLoad { count, panes } => arguments.extend([
            "--count".to_string(),
            count.to_string(),
            "--plan".to_string(),
            plan_keys(&split_plan(*panes)),
            "--started-dir".to_string(),
            dirs.started.to_string_lossy().into_owned(),
        ]),
    }
    arguments.push("--".to_string());
    arguments.extend(app.command.iter().cloned());
    arguments
}

/// A directory that is removed when the run is over.
struct Scratch(PathBuf);

impl Scratch {
    /// A new directory in /tmp. The path is short because Benterm's socket
    /// is below it, and a socket's path may not be long.
    fn new() -> Result<Self> {
        let path = PathBuf::from(format!("/tmp/benterm-latency-{}", std::process::id()));
        fs::create_dir(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What a command prints, trimmed, or `unknown`.
fn printed(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The load averages of the last 1, 5 and 15 minutes.
fn load_average() -> String {
    printed("sysctl", &["-n", "vm.loadavg"])
        .trim_matches(['{', '}', ' '])
        .to_string()
}

/// The order the apps are started in for round `round`: every other
/// round the other way, so that neither app always follows the other.
pub fn start_order(apps: &[App], round: u32) -> Vec<App> {
    let mut order = apps.to_vec();
    if round % 2 == 1 {
        order.reverse();
    }
    order
}

/// The blocks `keys` keys of each app are pressed in. With two apps
/// each has two blocks, one app's around the other's, so that a machine
/// that gets busier or quieter during the run does so for both.
pub fn key_blocks(apps: &[App], keys: u32) -> Vec<(App, u32)> {
    if apps.len() < 2 {
        return apps.iter().map(|app| (*app, keys)).collect();
    }
    let first = keys.div_ceil(2);
    let second = keys - first;
    let forward = apps.iter().map(|app| (*app, first));
    let back = apps.iter().rev().map(|app| (*app, second));
    forward.chain(back).filter(|(_, keys)| *keys > 0).collect()
}

struct Run<'a> {
    options: &'a LatencyOptions,
    probe: PathBuf,
    benterm: PathBuf,
    ghostty: PathBuf,
    writer: PathBuf,
    scratch: Scratch,
    starts: u32,
    measurements: Vec<Measurement>,
}

impl Run<'_> {
    /// Starts `app` once for `probe` and returns what the probe measured.
    fn once(&mut self, app: App, probe: Probe) -> Result<ProbeOutput> {
        self.starts += 1;
        let dirs = RunDirs::in_dir(&self.scratch.0.join(self.starts.to_string()));
        dirs.write(self.options, &self.writer)?;
        let command = app_command(app, &dirs, &self.benterm, &self.ghostty);
        let load = load_average();
        let mut output = probe::run(&self.probe, &probe_arguments(&probe, &command, &dirs))?;
        output.info.insert("load_average_before".to_string(), load);
        if let Probe::KeysUnderLoad { .. } = probe {
            // What the writers wrote, as of their last full second.
            let (writers, progress) = read_progress(&dirs.started)?;
            let info = [
                ("writers", writers as u64),
                ("lines_written", progress.written),
                ("lines_skipped", progress.skipped),
            ];
            for (name, value) in info {
                output.info.insert(name.to_string(), value.to_string());
            }
        }
        Ok(output)
    }

    fn measure(&mut self) -> Result<()> {
        let options = self.options;
        let empty = |app: App, scenario| Measurement {
            app,
            scenario,
            samples: Vec::new(),
            misses: 0,
            info: Default::default(),
        };
        let of = |list: &[Measurement], app: App| -> usize {
            list.iter()
                .position(|m| m.app == app)
                .expect("every app has a row")
        };

        // The first start of each app is not counted: it loads what the
        // later ones find loaded.
        let mut starts: Vec<Measurement> = options
            .apps
            .iter()
            .map(|app| empty(*app, Scenario::Start))
            .collect();
        for round in 0..=options.starts {
            for app in start_order(&options.apps, round) {
                eprintln!("start {round} of {}: {}", options.starts, app.name());
                let output = self.once(app, Probe::Start)?;
                if round == 0 {
                    continue;
                }
                let at = of(&starts, app);
                let start = &mut starts[at];
                start.samples.extend(output.samples);
                // How many of the starts the probe had to bring to the front.
                let raised = output
                    .info
                    .get("raised_by_probe")
                    .is_some_and(|r| r == "true");
                let count = start
                    .info
                    .entry("raised_by_probe".to_string())
                    .or_insert("0".into());
                *count = (count.parse::<u32>().unwrap_or(0) + u32::from(raised)).to_string();
                if let Some(window) = output.info.get("window_points") {
                    start
                        .info
                        .insert("window_points".to_string(), window.clone());
                }
            }
        }
        self.measurements.extend(starts);

        for scenario in [Scenario::Keys, Scenario::KeysUnderLoad] {
            let mut rows: Vec<Measurement> = options
                .apps
                .iter()
                .map(|app| empty(*app, scenario))
                .collect();
            let mut block_of = vec![0; rows.len()];
            for (app, count) in key_blocks(&options.apps, options.keys) {
                eprintln!("{}: {}, {count} keys", scenario.title(), app.name());
                let probe = match scenario {
                    Scenario::KeysUnderLoad => Probe::KeysUnderLoad {
                        count,
                        panes: options.panes,
                    },
                    _ => Probe::Keys { count },
                };
                let output = self.once(app, probe)?;
                let at = of(&rows, app);
                block_of[at] += 1;
                rows[at].samples.extend(output.samples);
                rows[at].misses += output.misses;
                for (name, value) in output.info {
                    rows[at]
                        .info
                        .insert(format!("block{}.{name}", block_of[at]), value);
                }
            }
            self.measurements.extend(rows);
        }
        Ok(())
    }
}

/// The results as a Markdown document: where and when they were
/// measured, the table, and what the probe noted for each row.
fn document(context: &[(&str, String)], measurements: &[Measurement]) -> String {
    let mut document = String::from("# Benterm and Ghostty: key and start latency\n\n");
    for (name, value) in context {
        document.push_str(&format!("- {name}: {value}\n"));
    }
    document.push('\n');
    document.push_str(&table(measurements));
    document.push_str(
        "\nA time runs from the key press, or from the start of the app, to the moment the \
         display showed the frame with the result. The display shows a frame every 1000 / \
         display_hz milliseconds, so a result waits up to one frame for the display, and two \
         times closer than a frame are not told apart by one sample. The keys are pressed at \
         a different moment within a frame every time. A key whose character did not show \
         within a second is counted under Missed and is not in the other columns. A row whose \
         cpu_busy_percent or load average is high was measured on a busy machine: measure \
         again when it is quiet. frames_dropped counts frames the probe could not look at in \
         time; a time measured then may be a frame too long.\n\n",
    );
    for measurement in measurements {
        let notes: Vec<String> = measurement
            .info
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        document.push_str(&format!(
            "- {}, {}: {}\n",
            measurement.scenario.title(),
            measurement.app.name(),
            notes.join(", ")
        ));
    }
    document
}

/// Runs `cargo xtask latency` for the workspace at `workspace`, whose
/// build output is in `target`.
pub fn run(options: &LatencyOptions, workspace: &Path, target: &Path) -> Result<()> {
    let probe = probe::build(&target.join("latency/probe"))?;
    if !probe::permitted(&probe, true)? {
        bail!(
            "the terminal this runs in may not read the screen, press keys or move windows.\n\
             In System Settings > Privacy & Security, allow the terminal app under both\n\
             \"Screen & System Audio Recording\" and \"Accessibility\", restart the terminal,\n\
             and run this again."
        );
    }
    if options.check {
        println!("The permissions are there.");
        return Ok(());
    }
    let ghostty = if options.apps.contains(&App::Ghostty) {
        ghostty_copy(&options.ghostty, &target.join("latency/ghostty"))?
    } else {
        options.ghostty.clone()
    };
    let benterm = target.join("release/benterm");
    if options.apps.contains(&App::Benterm) {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "--release", "-p", "benterm"])
            .current_dir(workspace)
            .status()
            .context("run cargo build")?;
        if !status.success() {
            bail!("cargo build failed");
        }
    }

    let out = match &options.out {
        Some(out) => out.clone(),
        None => target
            .join("latency")
            .join(printed("date", &["+%Y%m%d-%H%M%S"])),
    };
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let ghostty_binary = ghostty_binary(&ghostty);
    let mut context =
        vec![
        ("Measured at", printed("date", &["+%Y-%m-%d %H:%M %z"])),
        (
            "Machine",
            printed("sysctl", &["-n", "machdep.cpu.brand_string"]),
        ),
        ("macOS", printed("sw_vers", &["-productVersion"])),
        (
            "Benterm",
            format!(
                "commit {}, the release binary started without a bundle",
                printed(
                    "git",
                    &["-C", &workspace.to_string_lossy(), "rev-parse", "--short", "HEAD"],
                )
            ),
        ),
        (
            "Ghostty",
            format!(
                "{}, a copy of the installed app under another bundle identifier",
                printed(&ghostty_binary.to_string_lossy(), &["+version"])
                    .lines()
                    .next()
                    .unwrap_or("unknown")
            ),
        ),
        ("Font", format!("{} {}", options.font, options.font_size)),
        (
            "Shell",
            "/bin/zsh as a login shell, with a prompt and nothing else in its .zshrc, and the \
             environment of the terminal the run was started in"
                .to_string(),
        ),
        (
            "Windows",
            "for the starts, as each app opens; for the keys, both at the same frame, of which \
             Benterm's sidebar takes 240 points"
                .to_string(),
        ),
        (
            "Busy panes",
            format!(
                "{} panes, {} lines a second each",
                options.panes, options.rate
            ),
        ),
        (
            "Load average at the start (1, 5, 15 minutes)",
            load_average(),
        ),
    ];

    let mut run = Run {
        options,
        probe,
        benterm,
        ghostty: ghostty_binary,
        writer: env::current_exe().context("find this program")?,
        scratch: Scratch::new()?,
        starts: 0,
        measurements: Vec::new(),
    };
    let measured = run.measure();
    context.push(("Load average at the end (1, 5, 15 minutes)", load_average()));

    // What was measured before a failure is still written.
    let results = out.join("results.md");
    fs::write(&results, document(&context, &run.measurements))
        .with_context(|| format!("write {}", results.display()))?;
    fs::write(out.join("samples.csv"), csv(&run.measurements))?;
    println!("{}", table(&run.measurements));
    println!("{}", results.display());
    measured
}

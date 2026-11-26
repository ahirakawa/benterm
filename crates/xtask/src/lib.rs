//! Builds Benterm.app from the `benterm` binary. `cargo xtask bundle` runs it.

pub mod icon;
pub mod latency;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use latency::redraws::RedrawOptions;
use latency::{App, LatencyOptions};

/// The bundle identifier of Benterm.app. macOS keys the notification
/// permission and other per-app settings by it, so it must not change.
pub const BUNDLE_ID: &str = "io.github.ahirakawa.benterm";

/// What Info.plist says about the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    pub identifier: String,
    /// The name Finder, the Dock and notifications show.
    pub name: String,
    /// The file name of the binary in `Contents/MacOS`.
    pub executable: String,
    pub version: String,
}

/// The file name of the icon in `Contents/Resources`.
pub const ICON_FILE: &str = "Benterm.icns";

/// Returns the `version` of the `[package]` table of a Cargo.toml.
pub fn package_version(cargo_toml: &str) -> Result<String> {
    let table: toml::Table = toml::from_str(cargo_toml).context("parse Cargo.toml")?;
    table
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .map(str::to_string)
        .context("Cargo.toml has no package.version")
}

/// Returns the Info.plist of the app as XML.
pub fn info_plist(info: &BundleInfo) -> String {
    let strings = [
        ("CFBundleIdentifier", info.identifier.as_str()),
        ("CFBundleName", &info.name),
        ("CFBundleDisplayName", &info.name),
        ("CFBundleExecutable", &info.executable),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", &info.version),
        ("CFBundleVersion", &info.version),
        ("CFBundleIconFile", ICON_FILE),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundleDevelopmentRegion", "en"),
        (
            "LSApplicationCategoryType",
            "public.app-category.developer-tools",
        ),
    ];
    let mut entries = String::new();
    for (key, value) in strings {
        entries.push_str(&format!(
            "\t<key>{key}</key>\n\t<string>{}</string>\n",
            escape(value)
        ));
    }
    entries.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n");
    // The built-in browser opens http pages. Only what web views load is
    // let past App Transport Security; the app's own connections are not.
    entries.push_str(
        "\t<key>NSAppTransportSecurity</key>\n\t<dict>\n\
         \t\t<key>NSAllowsArbitraryLoadsInWebContent</key>\n\t\t<true/>\n\
         \t</dict>\n",
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n{entries}</dict>\n</plist>\n"
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Writes the app to `app`, which must not exist yet.
pub fn assemble(app: &Path, binary: &Path, icon: &Path, info: &BundleInfo) -> Result<()> {
    if app.exists() {
        bail!("{} already exists", app.display());
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos).with_context(|| format!("create {}", macos.display()))?;
    fs::create_dir_all(&resources).with_context(|| format!("create {}", resources.display()))?;
    fs::write(contents.join("Info.plist"), info_plist(info)).context("write Info.plist")?;
    fs::write(contents.join("PkgInfo"), "APPL????").context("write PkgInfo")?;

    let installed = macos.join(&info.executable);
    fs::copy(binary, &installed).with_context(|| format!("copy {}", binary.display()))?;
    let mut permissions = fs::metadata(&installed)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(&installed, permissions)?;

    fs::copy(icon, resources.join(ICON_FILE))
        .with_context(|| format!("copy {}", icon.display()))?;
    Ok(())
}

/// Where in `Contents/Resources` the skill goes. `benterm setup` looks
/// for it there when it runs from the app.
pub const SKILL_DIR: &str = "skills/benterm";

/// Copies the files of the skill directory `skill` into the app's
/// Resources, so that the app has the skill wherever it is put.
pub fn add_skill(app: &Path, skill: &Path) -> Result<()> {
    let into = app.join("Contents/Resources").join(SKILL_DIR);
    fs::create_dir_all(&into).with_context(|| format!("create {}", into.display()))?;
    for entry in fs::read_dir(skill).with_context(|| format!("read {}", skill.display()))? {
        let path = entry?.path();
        if path.is_file() {
            let name = path.file_name().context("a file has a name")?;
            fs::copy(&path, into.join(name)).with_context(|| format!("copy {}", path.display()))?;
        }
    }
    Ok(())
}

/// The identity `codesign` takes for an ad-hoc signature, which needs no
/// certificate and is accepted on the Mac that made it.
pub const AD_HOC: &str = "-";

/// Signs the app with `identity`, a name from the keychain or [`AD_HOC`].
pub fn sign(app: &Path, identity: &str) -> Result<()> {
    // A timestamp needs Apple's server and an ad-hoc signature cannot carry
    // one, so it is left out for both.
    run(Command::new("codesign")
        .args([
            "--force",
            "--timestamp=none",
            "--options",
            "runtime",
            "--sign",
            identity,
        ])
        .arg(app))
}

/// Checks the app's signature with `codesign --verify`.
pub fn verify(app: &Path) -> Result<()> {
    run(Command::new("codesign")
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(app))
}

/// Packages the signed app and an Applications shortcut in a read-only image.
pub fn create_dmg(app: &Path, image: &Path) -> Result<()> {
    let release = image
        .parent()
        .context("DMG path needs a parent directory")?;
    let temp = tempfile::tempdir_in(release).context("create temporary DMG directory")?;
    let contents = temp.path().join("contents");
    fs::create_dir(&contents).context("create DMG staging directory")?;
    run(Command::new("/usr/bin/ditto")
        .arg(app)
        .arg(contents.join("Benterm.app")))?;
    std::os::unix::fs::symlink("/Applications", contents.join("Applications"))
        .context("create Applications shortcut")?;

    let temporary_image = temp.path().join("Benterm.dmg");
    run(Command::new("/usr/bin/hdiutil")
        .args([
            "create",
            "-volname",
            "Benterm",
            "-format",
            "UDZO",
            "-srcfolder",
        ])
        .arg(&contents)
        .arg(&temporary_image))?;
    fs::rename(temporary_image, image).with_context(|| format!("write {}", image.display()))
}

/// Runs a command, and turns a failure into an error with its output.
pub(crate) fn run(command: &mut Command) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("run {:?}", command.get_program()))?;
    if !output.status.success() {
        bail!(
            "{:?} failed: {}{}",
            command,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Where `cargo build` puts its output for the workspace at `workspace`:
/// `target_dir`, the value of `CARGO_TARGET_DIR`, resolved against the
/// workspace when relative, or `target` in the workspace when unset.
pub fn target_dir(workspace: &Path, target_dir: Option<&std::ffi::OsStr>) -> PathBuf {
    match target_dir {
        // `join` keeps an absolute path as it is.
        Some(dir) => workspace.join(dir),
        None => workspace.join("target"),
    }
}

/// Turns the arguments after `cargo xtask` into a command.
pub fn parse_args(args: &[String]) -> Result<Task> {
    let mut args = args.iter().map(String::as_str);
    match args.next() {
        None | Some("help" | "--help" | "-h") => Ok(Task::Help),
        Some(command @ ("bundle" | "dmg")) => {
            let mut runtime_shaders = false;
            let mut identity = AD_HOC.to_string();
            while let Some(arg) = args.next() {
                match arg {
                    "--runtime-shaders" => runtime_shaders = true,
                    "--sign" => {
                        identity = args.next().context("--sign needs an identity")?.to_string();
                    }
                    other => bail!("unknown option {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Task::Bundle {
                identity,
                runtime_shaders,
                dmg: command == "dmg",
            })
        }
        Some("flood") => {
            let (mut rate, mut pane, mut started_dir) = (100, 0, None);
            while let Some(arg) = args.next() {
                match arg {
                    "--rate" => {
                        rate = number(arg, args.next())?;
                        if !(1..=latency::flood::MAX_RATE).contains(&rate) {
                            bail!("--rate is from 1 to {}", latency::flood::MAX_RATE);
                        }
                    }
                    "--pane" => pane = number(arg, args.next())?,
                    "--started-dir" => {
                        started_dir = Some(PathBuf::from(value(arg, args.next())?));
                    }
                    other => bail!("unknown option {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Task::Flood {
                rate,
                pane,
                started_dir,
            })
        }
        Some("latency") => {
            let mut options = LatencyOptions::default();
            let mut apps = Vec::new();
            while let Some(arg) = args.next() {
                match arg {
                    "--keys" => options.keys = at_least_one(arg, args.next())?,
                    "--starts" => options.starts = at_least_one(arg, args.next())?,
                    "--rate" => {
                        options.rate = at_least_one(arg, args.next())?;
                        if options.rate > latency::flood::MAX_RATE {
                            bail!("--rate is from 1 to {}", latency::flood::MAX_RATE);
                        }
                    }
                    "--font" => options.font = value(arg, args.next())?.to_string(),
                    "--font-size" => options.font_size = number(arg, args.next())?,
                    "--app" => {
                        let name = value(arg, args.next())?;
                        let app = App::from_name(name).with_context(|| {
                            format!("--app is benterm or ghostty, not {name:?}")
                        })?;
                        if !apps.contains(&app) {
                            apps.push(app);
                        }
                    }
                    "--ghostty" => options.ghostty = PathBuf::from(value(arg, args.next())?),
                    "--out" => options.out = Some(PathBuf::from(value(arg, args.next())?)),
                    "--smoke" => (options.keys, options.starts) = (5, 2),
                    "--check" => options.check = true,
                    other => bail!("unknown option {other:?}\n\n{USAGE}"),
                }
            }
            if !apps.is_empty() {
                options.apps = apps;
            }
            Ok(Task::Latency(options))
        }
        Some("redraws") => {
            let mut options = RedrawOptions::default();
            while let Some(arg) = args.next() {
                match arg {
                    "--seconds" => options.seconds = at_least_one(arg, args.next())?,
                    "--panes" => options.panes = at_least_one(arg, args.next())?,
                    "--rate" => {
                        options.rate = at_least_one(arg, args.next())?;
                        if options.rate > latency::flood::MAX_RATE {
                            bail!("--rate is from 1 to {}", latency::flood::MAX_RATE);
                        }
                    }
                    "--out" => options.out = Some(PathBuf::from(value(arg, args.next())?)),
                    "--smoke" => options.seconds = 3,
                    other => bail!("unknown option {other:?}\n\n{USAGE}"),
                }
            }
            Ok(Task::Redraws(options))
        }
        Some(other) => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

/// The value after the option `name`.
fn value<'a>(name: &str, value: Option<&'a str>) -> Result<&'a str> {
    value.with_context(|| format!("{name} needs a value"))
}

/// The number after the option `name`.
fn number<T: std::str::FromStr>(name: &str, given: Option<&str>) -> Result<T> {
    let given = value(name, given)?;
    given
        .parse()
        .ok()
        .with_context(|| format!("{name} needs a number, not {given:?}"))
}

/// The count after the option `name`, which is not 0.
fn at_least_one(name: &str, given: Option<&str>) -> Result<u32> {
    match number(name, given)? {
        0 => bail!("{name} is at least 1"),
        count => Ok(count),
    }
}

/// What `cargo xtask` was asked to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    /// Build Benterm.app.
    Bundle {
        /// The signing identity: [`AD_HOC`] unless `--sign` names another.
        identity: String,
        runtime_shaders: bool,
        dmg: bool,
    },
    /// Write lines at a fixed rate, as the made-up busy program.
    Flood {
        /// Lines a second.
        rate: u32,
        /// The number each line starts with.
        pane: u32,
        /// Where to leave a file that says this program runs.
        started_dir: Option<PathBuf>,
    },
    /// Measure key and start latency against Ghostty.
    Latency(LatencyOptions),
    /// Count the redraws that terminals behind the window cause in Benterm.
    Redraws(RedrawOptions),
    Help,
}

pub const USAGE: &str = "\
usage: cargo xtask bundle [--runtime-shaders] [--sign IDENTITY]
       cargo xtask dmg [--runtime-shaders] [--sign IDENTITY]
       cargo xtask latency [--check] [--smoke] [--keys N] [--starts N]
                           [--rate N] [--font NAME] [--font-size POINTS]
                           [--app benterm|ghostty] [--ghostty APP] [--out DIR]
       cargo xtask redraws [--smoke] [--seconds N] [--panes N] [--rate N]
                           [--out DIR]
       cargo xtask flood [--rate N] [--pane N] [--started-dir DIR]

bundle builds benterm in release mode and writes Benterm.app to the
release directory of the target directory. The app is signed ad hoc
unless --sign names a signing identity from the keychain.
With --runtime-shaders, GPUI compiles Metal shaders at startup.

dmg builds the same app and packages it with an Applications shortcut
in Benterm.dmg in the release directory.

latency measures Benterm and Ghostty from outside: the time from starting
the app to the prompt of its shell (--starts times, 10 by default), and
the time from a key press to the screen (--keys keys, 100 by default),
first in a window with one pane and then beside ten panes that each
write --rate lines a second (100 by default). It writes results.md and
samples.csv to --out, or to a new directory in latency/ of the target
directory, and prints the table. The terminal it runs in needs the
permissions to record the screen and to control the computer; --check
asks for them and measures nothing. --smoke is a short run of 5 keys and
2 starts. While it runs, the apps open in front and keys are typed into
them: leave the keyboard and the mouse alone. The Ghostty that runs is
a copy of --ghostty (/Applications/Ghostty.app by default) under another
bundle identifier, so the preferences of the installed one stay as they
are.

redraws counts inside Benterm what terminals behind the window cost it:
the frames the window draws, the redraws the terminals ask for, on view
and not, and the locks of the terminals' state each wake takes. It
counts --seconds (30 by default) with no output, then as long while
--panes agents (10 by default) started behind the window with agent run
each write --rate lines a second (100 by default). It writes redraws.md
to --out, or to a new directory in latency/ of the target directory,
and prints the table. --smoke counts 3 seconds each. Benterm's window
opens, but no key is typed and no permission is needed.

flood is the program in the ten panes: it writes lines at a fixed rate
until its terminal closes.";

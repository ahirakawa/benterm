//! `cargo xtask bundle` builds Benterm.app, and `cargo xtask latency`
//! measures Benterm against Ghostty, and `cargo xtask redraws` counts the
//! redraws inside Benterm. See [`xtask::USAGE`].

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use xtask::{BUNDLE_ID, BundleInfo, Task, USAGE, assemble, icon, package_version, parse_args};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = parse_args(&args).and_then(|task| match task {
        Task::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Task::Bundle {
            identity,
            runtime_shaders,
            dmg,
        } => bundle(&identity, runtime_shaders, dmg),
        Task::Flood {
            rate,
            pane,
            started_dir,
        } => xtask::latency::flood::run(rate, pane, started_dir.as_deref()).context("write lines"),
        Task::Latency(options) => {
            let workspace = workspace();
            let target = xtask::target_dir(&workspace, env::var_os("CARGO_TARGET_DIR").as_deref());
            xtask::latency::run::run(&options, &workspace, &target)
        }
        Task::Redraws(options) => {
            let workspace = workspace();
            let target = xtask::target_dir(&workspace, env::var_os("CARGO_TARGET_DIR").as_deref());
            xtask::latency::redraws::run(&options, &workspace, &target)
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn bundle(identity: &str, runtime_shaders: bool, dmg: bool) -> Result<()> {
    let workspace = workspace();
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build.args(["build", "--release", "-p", "benterm"]);
    if runtime_shaders {
        build.args(["--features", "runtime_shaders"]);
    }
    let status = build
        .current_dir(&workspace)
        .status()
        .context("run cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }

    let release =
        xtask::target_dir(&workspace, env::var_os("CARGO_TARGET_DIR").as_deref()).join("release");
    let manifest = workspace.join("crates/benterm/Cargo.toml");
    let version = package_version(
        &fs::read_to_string(&manifest).with_context(|| format!("read {}", manifest.display()))?,
    )?;
    let info = BundleInfo {
        identifier: BUNDLE_ID.to_string(),
        name: "Benterm".to_string(),
        executable: "benterm".to_string(),
        version,
    };

    let icon = release.join(xtask::ICON_FILE);
    icon::write_icns(&icon)?;
    let app = release.join("Benterm.app");
    if app.exists() {
        fs::remove_dir_all(&app).with_context(|| format!("remove {}", app.display()))?;
    }
    assemble(&app, &release.join("benterm"), &icon, &info)?;
    xtask::add_skill(&app, &workspace.join(xtask::SKILL_DIR))?;
    xtask::sign(&app, identity)?;
    xtask::verify(&app)?;
    let output = if dmg {
        let image = release.join("Benterm.dmg");
        xtask::create_dmg(&app, &image)?;
        image
    } else {
        app
    };
    println!("{}", output.display());
    Ok(())
}

/// The workspace root, two levels above this crate.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/xtask is inside the workspace")
        .to_path_buf()
}

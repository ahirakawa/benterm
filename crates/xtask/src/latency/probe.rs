//! Builds and runs the probe: the Swift program in `probe/probe.swift`
//! that presses the keys and reads the screen.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::report::{ProbeOutput, parse_probe};

/// The probe's source, kept in the binary so that it is built the same
/// from wherever the measurement is run.
const SOURCE: &str = include_str!("../../probe/probe.swift");

/// Builds the probe in `dir` with `swiftc`, which comes with Xcode, and
/// returns the binary. A probe already built from the same source is
/// kept.
pub fn build(dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let source = dir.join("probe.swift");
    let binary = dir.join("latency-probe");
    if binary.exists() && fs::read_to_string(&source).is_ok_and(|built| built == SOURCE) {
        return Ok(binary);
    }
    // The source is written last, so a build that failed is tried again.
    let _ = fs::remove_file(&source);
    let unbuilt = dir.join("probe-unbuilt.swift");
    fs::write(&unbuilt, SOURCE).with_context(|| format!("write {}", unbuilt.display()))?;
    let output = Command::new("swiftc")
        .arg("-O")
        .arg(&unbuilt)
        .arg("-o")
        .arg(&binary)
        .output()
        .context("run swiftc, which comes with Xcode")?;
    if !output.status.success() {
        bail!(
            "swiftc could not build the probe:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::rename(&unbuilt, &source).with_context(|| format!("write {}", source.display()))?;
    Ok(binary)
}

/// Runs the probe and reads what it measured. The probe's complaints go
/// to this program's standard error as they come.
pub fn run(binary: &Path, arguments: &[String]) -> Result<ProbeOutput> {
    let output = Command::new(binary)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("run {}", binary.display()))?;
    let measured = parse_probe(&String::from_utf8_lossy(&output.stdout))?;
    if !output.status.success() {
        bail!("the probe gave up ({})", output.status);
    }
    Ok(measured)
}

/// Whether the terminal this runs in may read the screen, press keys
/// and move windows. With `ask`, macOS is made to ask for what is
/// missing.
pub fn permitted(binary: &Path, ask: bool) -> Result<bool> {
    let mut command = Command::new(binary);
    command.arg("check");
    if ask {
        command.arg("--ask");
    }
    let output = command
        .output()
        .with_context(|| format!("run {}", binary.display()))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(output.status.success())
}

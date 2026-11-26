//! Builds `helper/main.rs`, which `src/helper.rs` embeds.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let source = "helper/main.rs";
    println!("cargo:rerun-if-changed={source}");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("benterm-pty-helper");
    let status = Command::new(env::var_os("RUSTC").unwrap())
        .args(["--edition", "2024", "--crate-type", "bin"])
        .args([
            "-C",
            "opt-level=s",
            "-C",
            "panic=abort",
            "-C",
            "strip=symbols",
        ])
        .args(["--target", &env::var("TARGET").unwrap()])
        .arg("-o")
        .arg(&out)
        .arg(source)
        .status()
        .expect("run rustc for the helper");
    assert!(status.success(), "building {source} failed");
}

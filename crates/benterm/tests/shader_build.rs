//! The default build compiles GPUI's Metal shaders ahead of time.

use std::process::Command;

#[test]
fn default_build_does_not_enable_runtime_shaders() {
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--offline",
            "--edges",
            "features",
            "--invert",
            "gpui_platform",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        !tree.contains("gpui_platform feature \"runtime_shaders\""),
        "the default build must compile Metal shaders ahead of time:\n{tree}"
    );
}

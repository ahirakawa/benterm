use std::process::Command;

use xtask::latency::probe;

/// The probe is Swift, because the screen is read with ScreenCaptureKit.
/// Its own checks run on pictures it makes in memory, so they need
/// neither a screen nor a permission. A probe that does not compile
/// fails this test.
#[test]
fn the_probe_builds_and_passes_its_own_checks() {
    let dir = tempfile::tempdir().unwrap();
    let binary = probe::build(dir.path()).unwrap();
    let output = Command::new(&binary).arg("selftest").output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "selftest ok\n");
}

#[test]
fn the_probe_is_built_once_for_the_same_source() {
    let dir = tempfile::tempdir().unwrap();
    let binary = probe::build(dir.path()).unwrap();
    let built = std::fs::metadata(&binary).unwrap().modified().unwrap();
    assert_eq!(probe::build(dir.path()).unwrap(), binary);
    assert_eq!(
        std::fs::metadata(&binary).unwrap().modified().unwrap(),
        built
    );
}

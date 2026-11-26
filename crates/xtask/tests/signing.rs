use std::fs;
use std::path::{Path, PathBuf};

use xtask::{AD_HOC, BUNDLE_ID, BundleInfo, assemble, sign, verify};

/// Lays out an app around a real Mach-O binary, since `codesign` signs
/// only those.
fn unsigned_app(dir: &Path) -> PathBuf {
    let icon = dir.join("icon.icns");
    fs::write(&icon, b"icon").unwrap();
    let app = dir.join("Benterm.app");
    let info = BundleInfo {
        identifier: BUNDLE_ID.to_string(),
        name: "Benterm".to_string(),
        executable: "benterm".to_string(),
        version: "0.1.0".to_string(),
    };
    assemble(&app, Path::new("/usr/bin/true"), &icon, &info).unwrap();
    app
}

#[test]
fn an_ad_hoc_signed_app_passes_verification() {
    let dir = tempfile::tempdir().unwrap();
    let app = unsigned_app(dir.path());
    sign(&app, AD_HOC).unwrap();
    verify(&app).unwrap();
}

#[test]
fn an_ad_hoc_signed_app_has_hardened_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let app = unsigned_app(dir.path());
    sign(&app, AD_HOC).unwrap();

    let output = std::process::Command::new("codesign")
        .args(["--display", "--verbose=4"])
        .arg(&app)
        .output()
        .unwrap();
    let details = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{details}");
    assert!(
        details.lines().any(|line| line.contains("flags=")
            && line.split(['(', ',', ')']).any(|flag| flag == "runtime")),
        "{details}"
    );
}

#[test]
fn an_unsigned_app_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let app = unsigned_app(dir.path());
    assert!(verify(&app).is_err());
}

#[test]
fn an_app_changed_after_signing_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let app = unsigned_app(dir.path());
    sign(&app, AD_HOC).unwrap();
    fs::write(app.join("Contents/Resources/Benterm.icns"), b"changed").unwrap();
    assert!(verify(&app).is_err());
}

#[test]
fn signing_with_an_unknown_identity_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let app = unsigned_app(dir.path());
    assert!(sign(&app, "No Such Identity For Benterm Tests").is_err());
}

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use xtask::{BUNDLE_ID, BundleInfo, ICON_FILE, assemble, info_plist, package_version};

fn info() -> BundleInfo {
    BundleInfo {
        identifier: BUNDLE_ID.to_string(),
        name: "Benterm".to_string(),
        executable: "benterm".to_string(),
        version: "1.2.3".to_string(),
    }
}

/// Reads one key of a plist file with `plutil`, as macOS would see it.
fn plist_value(plist: &Path, key: &str) -> String {
    let out = Command::new("plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(plist)
        .output()
        .expect("run plutil");
    assert!(
        out.status.success(),
        "{key} is missing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn the_version_comes_from_the_package_table() {
    let toml = "[package]\nname = \"benterm\"\nversion = \"0.4.1\"\n\n[dependencies]\nfoo = { version = \"9\" }\n";
    assert_eq!(package_version(toml).unwrap(), "0.4.1");
}

#[test]
fn a_cargo_toml_without_a_version_is_an_error() {
    assert!(package_version("[package]\nname = \"x\"\n").is_err());
}

#[test]
fn the_info_plist_holds_the_keys_macos_needs_to_launch_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let plist = dir.path().join("Info.plist");
    fs::write(&plist, info_plist(&info())).unwrap();

    let lint = Command::new("plutil")
        .arg("-lint")
        .arg(&plist)
        .output()
        .unwrap();
    assert!(
        lint.status.success(),
        "{}",
        String::from_utf8_lossy(&lint.stdout)
    );

    for (key, value) in [
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleName", "Benterm"),
        ("CFBundleDisplayName", "Benterm"),
        ("CFBundleExecutable", "benterm"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", "1.2.3"),
        ("CFBundleVersion", "1.2.3"),
        ("CFBundleIconFile", ICON_FILE),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("NSHighResolutionCapable", "true"),
    ] {
        assert_eq!(plist_value(&plist, key), value, "{key}");
    }
}

/// The built-in browser opens http pages, which App Transport Security
/// would refuse. Only what web views load is let through: the app's own
/// connections stay under its rules.
#[test]
fn the_info_plist_lets_web_views_load_http_pages_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let plist = dir.path().join("Info.plist");
    fs::write(&plist, info_plist(&info())).unwrap();

    assert_eq!(
        plist_value(
            &plist,
            "NSAppTransportSecurity.NSAllowsArbitraryLoadsInWebContent"
        ),
        "true"
    );
    let out = Command::new("plutil")
        .args(["-extract", "NSAppTransportSecurity", "json", "-o", "-"])
        .arg(&plist)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        r#"{"NSAllowsArbitraryLoadsInWebContent":true}"#,
        "App Transport Security has more keys than the one for web views"
    );
}

#[test]
fn text_in_the_info_plist_is_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let plist = dir.path().join("Info.plist");
    let mut info = info();
    info.name = "Benterm <&> \"Dev\"".to_string();
    fs::write(&plist, info_plist(&info)).unwrap();
    assert_eq!(plist_value(&plist, "CFBundleName"), "Benterm <&> \"Dev\"");
}

#[test]
fn the_app_holds_the_info_plist_the_binary_and_the_icon() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("benterm");
    fs::write(&binary, b"binary").unwrap();
    let icon = dir.path().join("icon.icns");
    fs::write(&icon, b"icon").unwrap();
    let app = dir.path().join("Benterm.app");

    assemble(&app, &binary, &icon, &info()).unwrap();

    let contents = app.join("Contents");
    assert_eq!(
        plist_value(&contents.join("Info.plist"), "CFBundleExecutable"),
        "benterm"
    );
    let installed = contents.join("MacOS/benterm");
    assert_eq!(fs::read(&installed).unwrap(), b"binary");
    let mode = fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the binary is executable");
    assert_eq!(
        fs::read(contents.join("Resources").join(ICON_FILE)).unwrap(),
        b"icon"
    );
    assert_eq!(
        fs::read_to_string(contents.join("PkgInfo")).unwrap(),
        "APPL????"
    );
}

#[test]
fn assembling_over_an_existing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("benterm");
    fs::write(&binary, b"binary").unwrap();
    let icon = dir.path().join("icon.icns");
    fs::write(&icon, b"icon").unwrap();
    let app = dir.path().join("Benterm.app");
    fs::create_dir(&app).unwrap();

    assert!(assemble(&app, &binary, &icon, &info()).is_err());
}

#[test]
fn the_app_holds_the_skill_where_benterm_setup_looks_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("skills/benterm");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "the skill").unwrap();
    let app = dir.path().join("Benterm.app");
    fs::create_dir_all(app.join("Contents/Resources")).unwrap();

    xtask::add_skill(&app, &skill).unwrap();

    assert_eq!(
        fs::read_to_string(app.join("Contents/Resources/skills/benterm/SKILL.md")).unwrap(),
        "the skill"
    );
}

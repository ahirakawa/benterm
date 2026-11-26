//! GPUI draws text on macOS only with the `font-kit` feature.
//!
//! Without the feature, `gpui_macos` falls back to a text system that
//! measures and rasterizes nothing: the window opens and every glyph is
//! missing. The feature therefore has to stay on.

const WORKSPACE_MANIFEST: &str = include_str!("../../../Cargo.toml");

#[test]
fn gpui_platform_enables_font_kit() {
    let line = WORKSPACE_MANIFEST
        .lines()
        .find(|line| line.trim_start().starts_with("gpui_platform"))
        .expect("the workspace manifest declares gpui_platform");
    assert!(
        line.contains("\"font-kit\""),
        "gpui_platform must enable font-kit: {line}"
    );
}

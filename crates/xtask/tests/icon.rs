use std::fs;
use std::process::Command;

use xtask::icon::{render, write_icns};

#[test]
fn the_icon_is_square_with_transparent_corners_and_an_opaque_middle() {
    for size in [16, 32, 128, 1024] {
        let image = render(size);
        assert_eq!(image.dimensions(), (size, size));
        assert_eq!(image.get_pixel(0, 0)[3], 0, "corner at {size}");
        assert_eq!(
            image.get_pixel(size - 1, size - 1)[3],
            0,
            "corner at {size}"
        );
        assert_eq!(
            image.get_pixel(size / 2, size / 2)[3],
            255,
            "middle at {size}"
        );
    }
}

#[test]
fn the_icon_has_more_than_one_color() {
    let image = render(256);
    let first = *image.get_pixel(128, 128);
    assert!(
        image
            .pixels()
            .any(|pixel| pixel[3] == 255 && *pixel != first)
    );
}

#[test]
fn the_icns_file_holds_every_size_macos_asks_for() {
    let dir = tempfile::tempdir().unwrap();
    let icns = dir.path().join("Benterm.icns");
    write_icns(&icns).unwrap();

    // Unpacking with iconutil shows the file is one macOS can read.
    let iconset = dir.path().join("unpacked.iconset");
    let status = Command::new("iconutil")
        .args(["--convert", "iconset", "--output"])
        .arg(&iconset)
        .arg(&icns)
        .status()
        .unwrap();
    assert!(status.success());
    for name in [
        "icon_16x16.png",
        "icon_16x16@2x.png",
        "icon_32x32.png",
        "icon_32x32@2x.png",
        "icon_128x128.png",
        "icon_128x128@2x.png",
        "icon_256x256.png",
        "icon_256x256@2x.png",
        "icon_512x512.png",
        "icon_512x512@2x.png",
    ] {
        assert!(
            fs::metadata(iconset.join(name)).is_ok(),
            "{name} is missing"
        );
    }
}

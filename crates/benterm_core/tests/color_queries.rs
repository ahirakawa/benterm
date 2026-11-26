//! Answers to color queries (OSC 4, 10, 11 and 12).

use benterm_core::{AlacrittyCore, GridSize, ReportedColors, Rgb, TerminalCore};

fn colors() -> ReportedColors {
    let mut palette = [Rgb(0, 0, 0); 256];
    palette[1] = Rgb(0xef, 0x6f, 0x6f);
    palette[255] = Rgb(0xee, 0xee, 0xee);
    ReportedColors {
        foreground: Rgb(0xd4, 0xd6, 0xdb),
        background: Rgb(0x1a, 0x1b, 0x1e),
        cursor: Rgb(0x01, 0x02, 0x03),
        palette,
    }
}

fn core() -> AlacrittyCore {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.set_reported_colors(colors());
    core
}

fn reply(core: &mut AlacrittyCore, query: &[u8]) -> String {
    core.feed(query);
    String::from_utf8(core.take_pty_replies()).unwrap()
}

#[test]
fn without_colors_a_query_gets_no_answer() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    assert_eq!(reply(&mut core, b"\x1b]11;?\x07"), "");
}

#[test]
fn osc_10_reports_the_foreground() {
    assert_eq!(
        reply(&mut core(), b"\x1b]10;?\x07"),
        "\x1b]10;rgb:d4d4/d6d6/dbdb\x07"
    );
}

#[test]
fn osc_11_reports_the_background() {
    assert_eq!(
        reply(&mut core(), b"\x1b]11;?\x07"),
        "\x1b]11;rgb:1a1a/1b1b/1e1e\x07"
    );
}

#[test]
fn osc_12_reports_the_cursor_color() {
    assert_eq!(
        reply(&mut core(), b"\x1b]12;?\x07"),
        "\x1b]12;rgb:0101/0202/0303\x07"
    );
}

#[test]
fn osc_4_reports_palette_entries() {
    assert_eq!(
        reply(&mut core(), b"\x1b]4;1;?\x07"),
        "\x1b]4;1;rgb:efef/6f6f/6f6f\x07"
    );
    assert_eq!(
        reply(&mut core(), b"\x1b]4;255;?\x07"),
        "\x1b]4;255;rgb:eeee/eeee/eeee\x07"
    );
}

#[test]
fn the_answer_ends_the_way_the_query_did() {
    assert_eq!(
        reply(&mut core(), b"\x1b]11;?\x1b\\"),
        "\x1b]11;rgb:1a1a/1b1b/1e1e\x1b\\"
    );
}

/// Programs send a color query followed by a device attributes query and
/// read until the second answer, so the answers must keep that order.
#[test]
fn answers_keep_the_order_of_the_queries() {
    assert_eq!(
        reply(&mut core(), b"\x1b]11;?\x07\x1b[c"),
        "\x1b]11;rgb:1a1a/1b1b/1e1e\x07\x1b[?6c"
    );
}

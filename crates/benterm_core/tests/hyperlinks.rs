//! OSC 8 hyperlinks: text whose cells carry an address of their own.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(4, 20), 100)
}

/// The address each cell of the first row carries, as text.
fn links(core: &AlacrittyCore) -> Vec<Option<String>> {
    core.snapshot().rows[0]
        .cells
        .iter()
        .map(|cell| cell.hyperlink.as_deref().map(str::to_owned))
        .collect()
}

#[test]
fn the_text_of_a_hyperlink_carries_its_address() {
    let mut core = core();
    core.feed(b"a \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ b");

    let links = links(&core);
    let url = Some("https://example.com".to_owned());
    assert_eq!(links[..2], [None, None]);
    assert_eq!(links[2..6], [url.clone(), url.clone(), url.clone(), url]);
    assert_eq!(links[6..8], [None, None]);
}

#[test]
fn a_hyperlink_ended_by_bel_and_with_an_id_carries_its_address() {
    let mut core = core();
    core.feed(b"\x1b]8;id=7;http://localhost:3000/x\x07go\x1b]8;;\x07");

    let links = links(&core);
    let url = Some("http://localhost:3000/x".to_owned());
    assert_eq!(links[..3], [url.clone(), url, None]);
}

#[test]
fn plain_text_carries_no_address() {
    let mut core = core();
    core.feed(b"see https://example.com");

    assert!(links(&core).iter().all(Option::is_none));
}

#[test]
fn a_hyperlink_that_scrolled_off_keeps_its_address() {
    let mut core = core();
    core.feed(b"\x1b]8;;https://example.com\x1b\\old\x1b]8;;\x1b\\\r\n\n\n\n\n");

    let row = &core.scrollback_rows(0..1)[0];
    assert_eq!(
        row.cells[0].hyperlink.as_deref(),
        Some("https://example.com")
    );
    assert_eq!(row.cells[3].hyperlink, None);
}

#[test]
fn cells_share_the_address_of_an_open_hyperlink() {
    let mut core = core();
    core.feed(b"\x1b]8;;https://example.com\x1b\\first\r\nsecond");
    let screen = core.snapshot();
    let link = screen.rows[0].cells[0].hyperlink.as_ref().unwrap();
    for cell in screen.rows[0].cells[..5]
        .iter()
        .chain(&screen.rows[1].cells[..6])
    {
        assert!(std::sync::Arc::ptr_eq(
            link,
            cell.hyperlink.as_ref().unwrap()
        ));
    }

    core.feed(b"\r\nthird\r\nfourth\r\nfifth\r\nsixth");
    let history = core.scrollback_rows(0..2);
    let first = history[0].cells[0].hyperlink.as_ref().unwrap();
    let second = history[1].cells[0].hyperlink.as_ref().unwrap();
    assert!(std::sync::Arc::ptr_eq(first, second));
}

#[test]
fn links_too_long_for_the_browser_are_absent_from_the_cells() {
    let mut core = core();
    let prefix = "https://example.com/";
    let address = format!("{prefix}{}", "x".repeat(8193 - prefix.len()));
    core.feed(format!("\x1b]8;;{address}\x07text").as_bytes());
    assert_eq!(core.snapshot().rows[0].cells[0].ch, 't');
    assert!(links(&core).iter().all(Option::is_none));
}

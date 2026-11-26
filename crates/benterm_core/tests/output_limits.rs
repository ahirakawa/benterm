//! Bytes that are only shown must not make the core work or keep memory
//! out of proportion to their number.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore, TerminalEvent};

/// The most characters a title or the title of a notification keeps.
const TITLE_CHARS: usize = 256;
/// The most characters the body of a notification keeps.
const BODY_CHARS: usize = 1024;
/// The most bytes of one OSC sequence that are read.
const OSC_BYTES: usize = 64 * 1024;
/// The most combining characters one cell keeps.
const ZERO_WIDTH_CHARS: usize = 32;

fn titles(core: &mut AlacrittyCore) -> Vec<String> {
    core.take_events()
        .into_iter()
        .filter_map(|event| match event {
            TerminalEvent::TitleChanged(title) => Some(title),
            _ => None,
        })
        .collect()
}

#[test]
fn a_repeat_writes_no_more_characters_than_the_screen_has_cells() {
    let size = GridSize::new(50, 200);
    let mut core = AlacrittyCore::new(size, 10_000);
    core.feed(b"a\x1b[65535b");

    // 65,535 characters would fill 328 rows of 200 columns. A screen of
    // them is all that can be seen, so at most the 50 rows of one screen
    // go to the scrollback.
    assert!(
        core.scrollback_len() <= 50,
        "{} rows scrolled",
        core.scrollback_len()
    );
    // The `a` and a screen of repeats: one more than the screen holds,
    // which begins the last row.
    let snapshot = core.snapshot();
    let (last, full) = snapshot.rows.split_last().unwrap();
    assert!(full.iter().all(|row| row.text() == "a".repeat(200)));
    assert_eq!(last.text().trim_end(), "a");
}

#[test]
fn a_repeat_within_the_screen_is_done_in_full() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(b"a\x1b[9b");
    assert_eq!(core.snapshot().rows[0].text().trim_end(), "aaaaaaaaaa");
    assert_eq!(core.scrollback_len(), 0);
}

#[test]
fn a_repeat_split_between_feeds_is_limited_too() {
    let mut core = AlacrittyCore::new(GridSize::new(50, 200), 10_000);
    core.feed(b"a\x1b[655");
    core.feed(b"35b");
    assert!(core.scrollback_len() <= 50);
}

#[test]
fn a_repeated_combining_character_does_not_pile_up_on_a_cell() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed("a\u{301}\x1b[65535b".as_bytes());
    let cell = &core.snapshot().rows[0].cells[0];
    assert_eq!(cell.ch, 'a');
    assert_eq!(cell.zero_width.len(), ZERO_WIDTH_CHARS);
}

#[test]
fn a_long_title_is_cut() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b't', 64 * 1024));
    bytes.push(0x07);
    core.feed(&bytes);

    assert_eq!(titles(&mut core), ["t".repeat(TITLE_CHARS)]);
}

#[test]
fn a_title_is_cut_by_characters_not_bytes() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(format!("\x1b]2;{}\x1b\\", "あ".repeat(1000)).as_bytes());
    assert_eq!(titles(&mut core), ["あ".repeat(TITLE_CHARS)]);
}

#[test]
fn a_title_within_the_limit_is_kept_whole() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let title = "t".repeat(TITLE_CHARS);
    core.feed(format!("\x1b]0;{title}\x07").as_bytes());
    assert_eq!(titles(&mut core), [title]);
}

#[test]
fn the_title_stack_keeps_the_cut_title() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b't', 64 * 1024));
    bytes.push(0x07);
    // What the stack holds is what a pop gives back.
    bytes.extend(b"\x1b[22t".repeat(4096));
    bytes.extend(b"\x1b]0;other\x07\x1b[23t");
    core.feed(&bytes);

    let titles = titles(&mut core);
    assert_eq!(
        titles,
        [
            "t".repeat(TITLE_CHARS),
            "other".to_owned(),
            "t".repeat(TITLE_CHARS)
        ]
    );
}

#[test]
fn text_after_a_cut_osc_is_shown() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b't', 2 * OSC_BYTES));
    bytes.extend(b"\x1b\\shown");
    core.feed(&bytes);
    assert_eq!(core.snapshot().rows[0].text().trim_end(), "shown");
}

#[test]
fn an_endless_title_is_cut_when_it_ends() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(b"\x1b]0;");
    let piece = vec![b't'; 1 << 20];
    for _ in 0..64 {
        core.feed(&piece);
    }
    core.feed(b"\x07");
    assert_eq!(titles(&mut core), ["t".repeat(TITLE_CHARS)]);
}

#[test]
fn a_long_notification_is_cut() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let mut bytes = b"\x1b]9;".to_vec();
    bytes.extend(std::iter::repeat_n(b'n', 8 << 20));
    bytes.push(0x07);
    core.feed(&bytes);
    assert_eq!(
        core.take_events(),
        [TerminalEvent::Notification {
            title: None,
            body: "n".repeat(BODY_CHARS)
        }]
    );

    let long = "あ".repeat(5000);
    core.feed(format!("\x1b]777;notify;{long};{long}\x07").as_bytes());
    assert_eq!(
        core.take_events(),
        [TerminalEvent::Notification {
            title: Some("あ".repeat(TITLE_CHARS)),
            body: "あ".repeat(BODY_CHARS)
        }]
    );
}

#[test]
fn combining_characters_beyond_the_limit_are_dropped_from_a_cell() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let mut text = String::from("a");
    text.extend(std::iter::repeat_n('\u{301}', 2_000_000));
    text.push('b');
    core.feed(text.as_bytes());

    let snapshot = core.snapshot();
    let cells = &snapshot.rows[0].cells;
    assert_eq!(cells[0].ch, 'a');
    assert_eq!(cells[0].zero_width, ['\u{301}'; ZERO_WIDTH_CHARS]);
    assert_eq!(cells[1].ch, 'b');
}

#[test]
fn coming_back_to_a_cell_does_not_add_beyond_the_limit() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let marks = "\u{301}".repeat(20);
    // The cursor is put after the `a` again before each run of marks.
    for _ in 0..10 {
        core.feed(format!("\x1b[1;1Ha{marks}\x1b[2;1Hb\x1b[1;2H{marks}").as_bytes());
    }
    let cell = &core.snapshot().rows[0].cells[0];
    assert_eq!(cell.zero_width.len(), ZERO_WIDTH_CHARS);
}

#[test]
fn a_cut_cell_keeps_its_color_and_its_link() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    let marks = "\u{301}".repeat(100);
    core.feed(format!("\x1b]8;;https://example.com\x07\x1b[31;4;58;5;2ma{marks}").as_bytes());
    let cell = &core.snapshot().rows[0].cells[0];
    assert_eq!(cell.zero_width.len(), ZERO_WIDTH_CHARS);
    assert_eq!(cell.fg, benterm_core::Color::Indexed(1));
    assert_eq!(cell.hyperlink.as_deref(), Some("https://example.com"));
}

#[test]
fn combining_characters_on_a_wide_character_are_limited() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(format!("あ{}", "\u{301}".repeat(100)).as_bytes());
    let cell = &core.snapshot().rows[0].cells[0];
    assert_eq!(cell.ch, 'あ');
    assert_eq!(cell.zero_width.len(), ZERO_WIDTH_CHARS);
}

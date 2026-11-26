//! Finding the web addresses that a terminal shows, for a command-click
//! to open.

use std::path::{Path, PathBuf};

use benterm_core::{AlacrittyCore, GridSize, Row, TerminalCore};
use benterm_ui::links::{
    Link, PathCandidate, find_urls, link_at, path_at, resolve_path, shown_target,
};
use benterm_ui::selection::Point;

/// The addresses `text` holds.
fn urls(text: &str) -> Vec<&str> {
    find_urls(text)
        .into_iter()
        .map(|range| &text[range])
        .collect()
}

/// The rows a terminal of `cols` columns shows after `bytes`.
fn rows(cols: u16, bytes: &[u8]) -> Vec<Row> {
    let mut core = AlacrittyCore::new(GridSize::new(4, cols), 0);
    core.feed(bytes);
    core.snapshot().rows
}

fn point(line: usize, col: u16) -> Point {
    Point { line, col }
}

#[test]
fn a_full_stop_after_an_address_is_not_part_of_it() {
    assert_eq!(
        urls("see https://example.com/a?b=1."),
        ["https://example.com/a?b=1"]
    );
}

#[test]
fn a_closing_parenthesis_around_an_address_is_not_part_of_it() {
    assert_eq!(urls("(https://example.com/x)"), ["https://example.com/x"]);
    assert_eq!(
        urls("[docs](https://example.com/x)."),
        ["https://example.com/x"]
    );
}

#[test]
fn parentheses_inside_an_address_are_part_of_it() {
    let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
    assert_eq!(urls(&format!("({url})")), [url]);
}

#[test]
fn punctuation_and_quotes_at_the_end_are_not_part_of_it() {
    assert_eq!(
        urls("go to http://localhost:3000, then"),
        ["http://localhost:3000"]
    );
    assert_eq!(urls("'https://example.com/a'"), ["https://example.com/a"]);
    assert_eq!(
        urls("\"https://example.com/a\"!"),
        ["https://example.com/a"]
    );
    assert_eq!(urls("<https://example.com/a>"), ["https://example.com/a"]);
}

#[test]
fn an_address_ends_where_japanese_text_begins() {
    assert_eq!(
        urls("https://example.com/aを開く"),
        ["https://example.com/a"]
    );
}

#[test]
fn several_addresses_are_all_found() {
    assert_eq!(
        urls("http://a.example https://b.example/x?y=1#z"),
        ["http://a.example", "https://b.example/x?y=1#z"]
    );
}

#[test]
fn text_that_is_no_web_address_has_none() {
    assert!(urls("ftp://example.com file:///tmp/x").is_empty());
    assert!(urls("https:// and http:///x").is_empty());
    assert!(urls("xhttps://example.com").is_empty());
}

#[test]
fn the_address_under_a_cell_is_found_with_the_cells_it_covers() {
    let rows = rows(40, b"see https://example.com/a?b=1.");

    let link = link_at(&rows[..1], 0, point(0, 10)).unwrap();

    assert_eq!(
        link,
        Link {
            url: "https://example.com/a?b=1".into(),
            start: point(0, 4),
            end: point(0, 28),
        }
    );
    assert_eq!(link_at(&rows[..1], 0, point(0, 2)), None);
    // The full stop is not part of the address.
    assert_eq!(link_at(&rows[..1], 0, point(0, 29)), None);
}

#[test]
fn an_address_wrapped_onto_two_rows_is_one_address() {
    let url = format!("https://example.com/{}", "a".repeat(80));
    assert_eq!(url.len(), 100);
    let rows = rows(80, url.as_bytes());

    for at in [point(5, 3), point(6, 10)] {
        let link = link_at(&rows[..2], 5, at).unwrap();
        assert_eq!(link.url, url);
        assert_eq!((link.start, link.end), (point(5, 0), point(6, 19)));
    }
}

#[test]
fn an_address_after_wide_characters_covers_the_right_cells() {
    let rows = rows(40, "日本 https://example.com".as_bytes());

    let link = link_at(&rows[..1], 0, point(0, 6)).unwrap();

    assert_eq!(link.url, "https://example.com");
    assert_eq!((link.start, link.end), (point(0, 5), point(0, 23)));
}

#[test]
fn the_text_of_a_hyperlink_opens_its_address() {
    let rows = rows(
        40,
        b"a \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ b",
    );

    let link = link_at(&rows[..1], 0, point(0, 3)).unwrap();

    assert_eq!(
        link,
        Link {
            url: "https://example.com".into(),
            start: point(0, 2),
            end: point(0, 5),
        }
    );
    assert_eq!(link_at(&rows[..1], 0, point(0, 7)), None);
}

#[test]
fn a_hyperlink_wrapped_onto_two_rows_is_one_link() {
    let rows = rows(
        10,
        b"12345678\x1b]8;;https://example.com\x1b\\abcd\x1b]8;;\x1b\\",
    );

    let link = link_at(&rows[..2], 0, point(1, 1)).unwrap();

    assert_eq!(link.url, "https://example.com");
    assert_eq!((link.start, link.end), (point(0, 8), point(1, 1)));
}

#[test]
fn a_hyperlink_to_something_other_than_a_web_page_is_not_opened() {
    let rows = rows(40, b"\x1b]8;;file:///etc/hosts\x1b\\hosts\x1b]8;;\x1b\\");

    assert_eq!(link_at(&rows[..1], 0, point(0, 1)), None);
    assert_eq!(path_at(&rows[..1], 0, point(0, 1)), None);
}

#[test]
fn a_file_path_under_a_cell_is_found_without_surrounding_punctuation() {
    let rows = rows(40, b"see (src/main.rs).");

    let candidate = path_at(&rows[..1], 0, point(0, 8)).unwrap();

    assert_eq!(
        candidate,
        PathCandidate {
            path: "src/main.rs".into(),
            start: point(0, 5),
            end: point(0, 15),
        }
    );
    assert_eq!(path_at(&rows[..1], 0, point(0, 4)), None);
    assert_eq!(path_at(&rows[..1], 0, point(0, 16)), None);
    assert_eq!(path_at(&rows[..1], 0, point(0, 17)), None);
}

#[test]
fn line_and_column_numbers_are_stripped_from_path_candidates() {
    let rows = rows(50, b"error at src/main.rs:12 and src/lib.rs:34:5:");

    let with_line = path_at(&rows[..1], 0, point(0, 22)).unwrap();
    assert_eq!(
        with_line,
        PathCandidate {
            path: "src/main.rs".into(),
            start: point(0, 9),
            end: point(0, 22),
        }
    );

    let with_line_col = path_at(&rows[..1], 0, point(0, 40)).unwrap();
    assert_eq!(
        with_line_col,
        PathCandidate {
            path: "src/lib.rs".into(),
            start: point(0, 28),
            end: point(0, 42),
        }
    );
}

#[test]
fn a_url_is_not_returned_as_a_path_candidate() {
    let rows = rows(40, b"see https://example.com/a");

    assert_eq!(path_at(&rows[..1], 0, point(0, 10)), None);
}

#[test]
fn resolve_path_normalizes_relative_and_trailing_slashes() {
    let base = Path::new("/work/project");
    assert_eq!(
        resolve_path("src/main.rs", Some(base)),
        Some(PathBuf::from("/work/project/src/main.rs"))
    );
    assert_eq!(
        resolve_path("./src/", Some(base)),
        Some(PathBuf::from("/work/project/src"))
    );
    assert_eq!(
        resolve_path("../other/file.txt", Some(base)),
        Some(PathBuf::from("/work/other/file.txt"))
    );
    assert_eq!(resolve_path("src/main.rs", None), None);
    assert_eq!(
        resolve_path("/tmp/example.txt", None),
        Some(PathBuf::from("/tmp/example.txt"))
    );
}

/// A user name before the host can push the host out of sight, as in
/// `https://github.com:login@evil.example/`. Such an address is no link.
#[test]
fn an_address_with_a_user_or_a_password_is_no_link() {
    for bytes in [
        &b"see https://github.com@evil.example/x now"[..],
        b"see https://github.com:login@evil.example/x now",
        b"see \x1b]8;;https://user@evil.example/\x1b\\a link\x1b]8;;\x1b\\ now",
    ] {
        let rows = rows(60, bytes);
        assert_eq!(link_at(&rows[..1], 0, point(0, 6)), None, "{bytes:?}");
    }
    let rows = rows(60, b"see https://evil.example/@github.com now");
    assert!(link_at(&rows[..1], 0, point(0, 6)).is_some());
}

/// The host comes first in what is shown of an address, so that a long
/// path does not push it out of the line, and a host written in letters
/// that look like others is shown as the host it is.
#[test]
fn the_host_of_an_address_is_shown_before_it() {
    assert_eq!(
        shown_target("https://other.example/login?next=/"),
        "other.example — https://other.example/login?next=/"
    );
    assert_eq!(
        shown_target("https://ex\u{430}mple.com/"),
        "xn--exmple-4nf.com — https://ex\u{430}mple.com/"
    );
    assert_eq!(
        shown_target("https://example.com/\u{202e}gpj.exe"),
        "example.com — https://example.com/gpj.exe"
    );
}

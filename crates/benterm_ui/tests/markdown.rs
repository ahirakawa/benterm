//! A Markdown file is shown formatted.

use std::fs;

use benterm_ui::file_preview::{Content, PREVIEW_LIMIT, read_preview};
use benterm_ui::markdown::{Block, Span, parse};

fn plain(text: &str) -> Span {
    Span {
        text: text.into(),
        ..Span::default()
    }
}

#[test]
fn headings_keep_their_level() {
    assert_eq!(
        parse("# Title\n\n### Part\n"),
        [
            Block::Heading {
                level: 1,
                spans: vec![plain("Title")]
            },
            Block::Heading {
                level: 3,
                spans: vec![plain("Part")]
            },
        ]
    );
}

#[test]
fn a_paragraph_keeps_its_inline_styles() {
    let blocks = parse("a **b** *c* `d` [e](https://example.com)");
    assert_eq!(
        blocks,
        [Block::Paragraph(vec![
            plain("a "),
            Span {
                bold: true,
                ..plain("b")
            },
            plain(" "),
            Span {
                italic: true,
                ..plain("c")
            },
            plain(" "),
            Span {
                code: true,
                ..plain("d")
            },
            plain(" "),
            Span {
                link: true,
                ..plain("e")
            },
        ])]
    );
}

#[test]
fn the_lines_of_a_paragraph_join_with_a_space() {
    assert_eq!(
        parse("one\ntwo\n"),
        [Block::Paragraph(vec![
            plain("one"),
            plain(" "),
            plain("two")
        ])]
    );
}

#[test]
fn list_items_have_markers_and_depth() {
    assert_eq!(
        parse("- a\n  - b\n\n1. c\n2. d\n"),
        [
            Block::Item {
                depth: 0,
                marker: "•".into(),
                spans: vec![plain("a")]
            },
            Block::Item {
                depth: 1,
                marker: "•".into(),
                spans: vec![plain("b")]
            },
            Block::Item {
                depth: 0,
                marker: "1.".into(),
                spans: vec![plain("c")]
            },
            Block::Item {
                depth: 0,
                marker: "2.".into(),
                spans: vec![plain("d")]
            },
        ]
    );
}

#[test]
fn a_code_block_keeps_its_lines() {
    assert_eq!(
        parse("```rust\nfn main() {\n    run();\n}\n```\n"),
        [Block::Code(vec![
            "fn main() {".into(),
            "    run();".into(),
            "}".into()
        ])]
    );
}

#[test]
fn quotes_and_rules() {
    assert_eq!(
        parse("> said\n\n---\n"),
        [Block::Quote(vec![plain("said")]), Block::Rule]
    );
}

#[test]
fn a_markdown_file_is_shown_formatted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("README.md");
    fs::write(&path, "# Hello\n").unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(
        preview.content,
        Content::Markdown(vec![Block::Heading {
            level: 1,
            spans: vec![plain("Hello")]
        }])
    );
}

#[test]
fn other_files_are_not_formatted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "# Hello\n").unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::Text("# Hello\n".into()));
}

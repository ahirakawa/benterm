//! Markdown blocks and their shared rendering for previews and side answers.

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString,
    Styled, StyledText, div, px,
};

use crate::terminal_view::hsla;
use crate::theme::SidebarTheme;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// A run of text with one style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

/// A block of a Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Levels 1 to 6.
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    /// An item of a list, `depth` lists deep, 0 for the outermost.
    Item {
        depth: usize,
        marker: String,
        spans: Vec<Span>,
    },
    /// A paragraph inside a block quote.
    Quote(Vec<Span>),
    /// The lines of a code block, as written.
    Code(Vec<String>),
    Rule,
}

/// The blocks of `text`. Tables are shown a row to a line, and HTML as
/// it is written.
pub fn parse(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(text, options) {
        builder.take(event);
    }
    builder.blocks
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    bold: usize,
    italic: usize,
    link: usize,
    quotes: usize,
    /// For each open list, the number of its next item if it is numbered.
    lists: Vec<Option<u64>>,
    /// The marker of the item whose text has not been shown yet.
    marker: Option<String>,
    /// The text of the code block being read.
    code: Option<String>,
}

impl Builder {
    fn take(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.code {
                Some(code) => code.push_str(&text),
                None => self.push_text(&text, false),
            },
            Event::Code(text) => self.push_text(&text, true),
            Event::InlineHtml(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push_text(&text, false)
            }
            Event::Html(text) => match &mut self.code {
                Some(code) => code.push_str(&text),
                None => self.code = Some(text.into_string()),
            },
            Event::SoftBreak | Event::HardBreak => self.push_text(" ", false),
            Event::Rule => {
                self.flush();
                self.blocks.push(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                self.marker = Some(if done { "☑" } else { "☐" }.into());
            }
            Event::FootnoteReference(_) => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::Heading { .. } => self.spans.clear(),
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Link { .. } => self.link += 1,
            Tag::BlockQuote(_) => {
                self.flush();
                self.quotes += 1;
            }
            Tag::List(first) => {
                // The text of the item the list is in comes before it.
                self.flush();
                self.lists.push(first);
            }
            Tag::Item => {
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}.", *number - 1)
                    }
                    _ => "•".into(),
                };
                self.marker = Some(marker);
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(level) => {
                let spans = std::mem::take(&mut self.spans);
                self.blocks.push(Block::Heading {
                    level: level as u8,
                    spans,
                });
            }
            TagEnd::Paragraph | TagEnd::Item | TagEnd::TableHead | TagEnd::TableRow => self.flush(),
            TagEnd::TableCell => self.push_text("  │  ", false),
            TagEnd::Strong => self.bold -= 1,
            TagEnd::Emphasis => self.italic -= 1,
            TagEnd::Link => self.link -= 1,
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes -= 1;
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => self.end_code(),
            _ => {}
        }
    }

    fn end_code(&mut self) {
        if let Some(code) = self.code.take() {
            let lines = code.lines().map(str::to_owned).collect();
            self.blocks.push(Block::Code(lines));
        }
    }

    fn push_text(&mut self, text: &str, code: bool) {
        self.spans.push(Span {
            text: text.to_owned(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            code,
            link: self.link > 0,
        });
    }

    /// Ends the block whose text has been read: the item it is in, a
    /// quote, or a paragraph. A table cell's separator after the last
    /// cell is dropped.
    fn flush(&mut self) {
        if self.code.is_some() {
            self.end_code();
        }
        if self.spans.last().is_some_and(|span| span.text == "  │  ") {
            self.spans.pop();
        }
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let block = match self.marker.take() {
            Some(marker) => Block::Item {
                depth: self.lists.len().saturating_sub(1),
                marker,
                spans,
            },
            None if self.quotes > 0 => Block::Quote(spans),
            None => Block::Paragraph(spans),
        };
        self.blocks.push(block);
    }
}

pub(crate) fn render_block(
    block: &Block,
    sidebar: &SidebarTheme,
    mono: &SharedString,
) -> AnyElement {
    let size = sidebar.font_size;
    match block {
        Block::Heading { level, spans } => {
            let scale = match level {
                1 => 1.6,
                2 => 1.35,
                3 => 1.15,
                _ => 1.0,
            };
            div()
                .mt(px(6.))
                .text_size(px(size * scale))
                .font_weight(FontWeight::BOLD)
                .child(styled(spans, sidebar))
                .into_any_element()
        }
        Block::Paragraph(spans) => div().child(styled(spans, sidebar)).into_any_element(),
        Block::Item {
            depth,
            marker,
            spans,
        } => div()
            .flex()
            .gap(px(6.))
            .pl(px(16. * *depth as f32))
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(sidebar.item_secondary_text))
                    .child(marker.clone()),
            )
            .child(div().flex_1().min_w_0().child(styled(spans, sidebar)))
            .into_any_element(),
        Block::Quote(spans) => div()
            .pl(px(10.))
            .border_l_2()
            .border_color(hsla(sidebar.border))
            .text_color(hsla(sidebar.item_secondary_text))
            .child(styled(spans, sidebar))
            .into_any_element(),
        Block::Code(lines) => div()
            .p(px(8.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .bg(hsla(sidebar.item_hover_background))
            .font_family(mono.clone())
            .overflow_hidden()
            .children(
                lines
                    .iter()
                    .map(|line| div().whitespace_nowrap().child(line.replace('\t', "    "))),
            )
            .into_any_element(),
        Block::Rule => div().h(px(1.)).bg(hsla(sidebar.border)).into_any_element(),
    }
}

/// `spans` as one text that wraps, each run in its style.
fn styled(spans: &[Span], sidebar: &SidebarTheme) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let style = HighlightStyle {
            font_weight: span.bold.then_some(FontWeight::BOLD),
            font_style: span.italic.then_some(FontStyle::Italic),
            color: span.link.then(|| hsla(sidebar.item_active_indicator)),
            background_color: span.code.then(|| hsla(sidebar.item_hover_background)),
            ..HighlightStyle::default()
        };
        if style != HighlightStyle::default() {
            highlights.push((start..text.len(), style));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

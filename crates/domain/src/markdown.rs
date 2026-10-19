//! Markdown turned into a small tree of blocks, ready to draw.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Text with one style. Adjacent spans differ in style.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    Code { language: String, text: String },
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<ListItem> },
    Table { header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
    /// A picture on a line of its own; `alt` is its description.
    Image { src: String, alt: String },
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    /// `Some(done)` for a task list entry.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Parse `source` into blocks.
pub fn parse_markdown(source: &str) -> Vec<Block> {
    parse_markdown_lines(source).into_iter().map(|(_, block)| block).collect()
}

/// Parse `source` into top-level blocks, each with the line (from 0) where it starts.
pub fn parse_markdown_lines(source: &str) -> Vec<(usize, Block)> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let line_starts: Vec<usize> =
        std::iter::once(0).chain(source.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut builder = Builder { line_starts, ..Builder::default() };
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        let starts_block = matches!(event, Event::Start(_) | Event::Rule);
        if starts_block && builder.frames.is_empty() && builder.pending.is_none() {
            builder.pending = Some(range.start);
        }
        builder.event(event);
    }
    builder.finish()
}

/// What is being built right now; the innermost is last.
enum Frame {
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<ListItem> },
    Item { checked: Option<bool>, blocks: Vec<Block> },
}

#[derive(Default)]
struct Builder {
    root: Vec<(usize, Block)>,
    line_starts: Vec<usize>,
    /// Byte where the top-level block being built starts.
    pending: Option<usize>,
    last_line: usize,
    frames: Vec<Frame>,
    spans: Vec<Span>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: Option<String>,
    image: Option<String>,
    code: Option<(String, String)>,
    heading: Option<u8>,
    table: Option<(Vec<Vec<Span>>, Vec<Vec<Vec<Span>>>, bool)>,
    row: Vec<Vec<Span>>,
}

impl Builder {
    fn push_block(&mut self, block: Block) {
        match self.frames.last_mut() {
            Some(Frame::Quote(blocks)) | Some(Frame::Item { blocks, .. }) => blocks.push(block),
            Some(Frame::List { .. }) => {}
            None => {
                let line = match self.pending.take() {
                    Some(byte) => self.line_starts.partition_point(|&start| start <= byte).saturating_sub(1),
                    None => self.last_line,
                };
                self.last_line = line;
                self.root.push((line, block));
            }
        }
    }

    fn push_text(&mut self, text: &str, code: bool) {
        let span = Span {
            text: text.to_owned(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
            link: self.link.clone(),
        };
        match self.spans.last_mut() {
            Some(last) if same_style(last, &span) => last.text.push_str(&span.text),
            _ => self.spans.push(span),
        }
    }

    /// Close the text gathered so far as a paragraph (items of tight lists have no paragraph tag).
    fn flush_paragraph(&mut self) {
        if !self.spans.is_empty() {
            let spans = std::mem::take(&mut self.spans);
            self.push_block(Block::Paragraph(spans));
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match self.code.as_mut() {
                Some((_, body)) => body.push_str(&text),
                None => self.push_text(&text, false),
            },
            Event::Code(text) => self.push_text(&text, true),
            Event::SoftBreak => self.push_text(" ", false),
            Event::HardBreak => self.push_text("\n", false),
            Event::Rule => {
                self.flush_paragraph();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item { checked, .. }) = self.frames.last_mut() {
                    *checked = Some(done);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(heading_number(level)),
            Tag::BlockQuote(_) => {
                self.flush_paragraph();
                self.frames.push(Frame::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                let language = match kind {
                    CodeBlockKind::Fenced(lang) => lang.split_whitespace().next().unwrap_or("").to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush_paragraph();
                self.frames.push(Frame::List { start, items: Vec::new() });
            }
            Tag::Item => self.frames.push(Frame::Item { checked: None, blocks: Vec::new() }),
            Tag::Table(_) => self.table = Some((Vec::new(), Vec::new(), true)),
            Tag::TableHead | Tag::TableRow => self.row.clear(),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.to_string()),
            Tag::Image { dest_url, .. } => {
                self.flush_paragraph();
                self.image = Some(dest_url.to_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_paragraph(),
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                let spans = std::mem::take(&mut self.spans);
                self.push_block(Block::Heading { level, spans });
            }
            TagEnd::BlockQuote(_) => {
                self.flush_paragraph();
                if let Some(Frame::Quote(blocks)) = self.frames.pop() {
                    self.push_block(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some((language, text)) = self.code.take() {
                    self.push_block(Block::Code { language, text: text.trim_end_matches('\n').to_owned() });
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List { start, items }) = self.frames.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.flush_paragraph();
                if let Some(Frame::Item { checked, blocks }) = self.frames.pop() {
                    if let Some(Frame::List { items, .. }) = self.frames.last_mut() {
                        items.push(ListItem { checked, blocks });
                    }
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.spans);
                self.row.push(cell);
            }
            TagEnd::TableHead => {
                if let Some((header, _, in_head)) = self.table.as_mut() {
                    *header = std::mem::take(&mut self.row);
                    *in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some((_, rows, _)) = self.table.as_mut() {
                    rows.push(std::mem::take(&mut self.row));
                }
            }
            TagEnd::Table => {
                if let Some((header, rows, _)) = self.table.take() {
                    self.push_block(Block::Table { header, rows });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = None,
            TagEnd::Image => {
                if let Some(src) = self.image.take() {
                    let alt = std::mem::take(&mut self.spans).iter().map(|s| s.text.as_str()).collect();
                    self.push_block(Block::Image { src, alt });
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<(usize, Block)> {
        self.flush_paragraph();
        self.root
    }
}

fn same_style(a: &Span, b: &Span) -> bool {
    (a.bold, a.italic, a.strike, a.code, &a.link) == (b.bold, b.italic, b.strike, b.code, &b.link)
}

fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn headings_and_paragraphs_keep_inline_styles() {
        let blocks = parse_markdown("# Title\n\nSome **bold** and *slanted* `code` [site](http://x.io).\n");
        let Block::Heading { level, spans } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!((*level, plain(spans).as_str()), (1, "Title"));
        let Block::Paragraph(spans) = &blocks[1] else { panic!() };
        assert_eq!(plain(spans), "Some bold and slanted code site.");
        assert!(spans.iter().any(|s| s.bold && s.text == "bold"));
        assert!(spans.iter().any(|s| s.italic && s.text == "slanted"));
        assert!(spans.iter().any(|s| s.code && s.text == "code"));
        assert!(spans.iter().any(|s| s.link.as_deref() == Some("http://x.io") && s.text == "site"));
    }

    #[test]
    fn fenced_code_keeps_its_language_and_lines() {
        let blocks = parse_markdown("```rust\nfn a() {}\n\nfn b() {}\n```\n");
        assert_eq!(
            blocks,
            vec![Block::Code { language: "rust".into(), text: "fn a() {}\n\nfn b() {}".into() }]
        );
    }

    #[test]
    fn lists_nest_and_track_tasks() {
        let blocks = parse_markdown("- one\n- [x] two\n  - inner\n\n3. three\n");
        let Block::List { start: None, items } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].checked, Some(true));
        assert!(matches!(items[1].blocks.last(), Some(Block::List { .. })));
        assert!(matches!(&blocks[1], Block::List { start: Some(3), .. }));
    }

    #[test]
    fn images_become_blocks_with_alt_text() {
        let blocks = parse_markdown("before\n\n![a *fine* day](pics/day.png)\n");
        assert_eq!(blocks[1], Block::Image { src: "pics/day.png".into(), alt: "a fine day".into() });
    }

    #[test]
    fn blocks_know_the_line_they_start_on() {
        let lines: Vec<usize> =
            parse_markdown_lines("# T\n\npara\ncontinues\n\n- a\n- b\n\n```\ncode\n```\n\n---\n").iter().map(|(l, _)| *l).collect();
        assert_eq!(lines, vec![0, 2, 5, 8, 12]);
    }

    #[test]
    fn quotes_rules_and_tables() {
        let blocks = parse_markdown("> quoted\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(matches!(&blocks[0], Block::Quote(inner) if matches!(inner[0], Block::Paragraph(_))));
        assert_eq!(blocks[1], Block::Rule);
        let Block::Table { header, rows } = &blocks[2] else { panic!("{blocks:?}") };
        assert_eq!((header.len(), rows.len(), plain(&rows[0][1]).as_str()), (2, 1, "2"));
    }
}

//! Rendered preview of a Markdown document, shown beside its editor.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    Pixels, canvas, div, img, point, prelude::*, px, rgb, AnyElement, Entity, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveText, SharedString, StrikethroughStyle, StyledText, UnderlineStyle,
};
use rustcode_application::IncrementalHighlighter;
use rustcode_domain::{parse_markdown_lines, Block, DocumentId, LineSpan, ListItem, Span, TextBuffer};

use crate::editor_view::EditorView;
use crate::groups::{PreviewMode, PreviewSync, SplitDir};
use crate::text_layout::{code_font, line_height, UI_FONT_SIZE};
use crate::theme::Theme;

/// Colour of links, as in VS Code's Markdown preview.
pub(crate) const LINK_COLOR: u32 = 0x3794ff;
/// Time allowed to colour one code block.
const CODE_BLOCK_BUDGET: std::time::Duration = std::time::Duration::from_millis(30);

pub(crate) fn is_markdown(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    title.ends_with(".md") || title.ends_with(".markdown")
}

/// Token spans of each line of one code block.
pub(crate) type CodeSpans = Vec<Vec<LineSpan>>;

/// A parsed document: its blocks and the colours of its code blocks, in drawing order.
pub(crate) struct MarkdownDoc {
    blocks: Vec<Block>,
    /// Line (from 0) where each block starts in the source.
    lines: Vec<usize>,
    code: Vec<CodeSpans>,
}

/// Parsed document of one revision, with the theme its code was coloured for.
pub(crate) type MarkdownCache = Option<(DocumentId, u64, usize, Rc<MarkdownDoc>)>;

/// Where a link leads.
#[derive(Clone)]
enum LinkTarget {
    Web(String),
    File(PathBuf),
    None,
}

fn link_target(url: &str, base: Option<&Path>) -> LinkTarget {
    if ["http://", "https://", "mailto:"].iter().any(|scheme| url.starts_with(scheme)) {
        return LinkTarget::Web(url.to_owned());
    }
    let path = url.split('#').next().unwrap_or("");
    match (path.is_empty(), base) {
        (false, Some(base)) => {
            let path = Path::new(path);
            LinkTarget::File(if path.is_absolute() { path.to_path_buf() } else { base.join(path) })
        }
        _ => LinkTarget::None,
    }
}

/// What drawing needs besides the blocks.
struct Draw<'a> {
    theme: &'a Theme,
    entity: Entity<EditorView>,
    base: Option<PathBuf>,
    code: &'a [CodeSpans],
    next_code: Cell<usize>,
    next_text: Cell<usize>,
}

pub(crate) fn code_blocks<'a>(blocks: &'a [Block], out: &mut Vec<(&'a str, &'a str)>) {
    for block in blocks {
        match block {
            Block::Code { language, text } => out.push((language, text)),
            Block::Quote(inner) => code_blocks(inner, out),
            Block::List { items, .. } => items.iter().for_each(|item| code_blocks(&item.blocks, out)),
            _ => {}
        }
    }
}

impl EditorView {
    /// The document parsed and coloured, redone only after an edit or a theme change.
    fn markdown_doc(&self, doc: DocumentId) -> Rc<MarkdownDoc> {
        let (document, _) = self.doc_ref(doc);
        let revision = document.editor.revision();
        let mut cache = self.markdown_cache.borrow_mut();
        if let Some((id, rev, theme, parsed)) = cache.as_ref() {
            if *id == doc && *rev == revision && *theme == self.theme_index {
                return parsed.clone();
            }
        }
        let (lines, blocks): (Vec<usize>, Vec<Block>) =
            parse_markdown_lines(&document.editor.buffer().text()).into_iter().unzip();
        let mut found = Vec::new();
        code_blocks(&blocks, &mut found);
        let code = found.into_iter().map(|(language, text)| self.highlight_code(language, text)).collect();
        let parsed = Rc::new(MarkdownDoc { blocks, lines, code });
        *cache = Some((doc, revision, self.theme_index, parsed.clone()));
        parsed
    }

    pub(crate) fn highlight_code(&self, language: &str, text: &str) -> CodeSpans {
        let session = self
            .syntax
            .detect_language(&format!("x.{language}"))
            .or_else(|| (!language.is_empty()).then(|| rustcode_domain::Language::new(language)))
            .and_then(|language| self.syntax.start_session(&language));
        let Some(session) = session else { return Vec::new() };
        let mut highlighter = IncrementalHighlighter::new(session, self.themes[self.theme_index].clone());
        let buffer = TextBuffer::from_text(text);
        highlighter.advance(&buffer, CODE_BLOCK_BUDGET);
        (0..buffer.line_count()).map(|i| highlighter.line(i).to_vec()).collect()
    }

    /// Markdown text drawn as blocks, for a notebook cell. Links are relative to `base`.
    pub(crate) fn render_markdown_fragment(&self, text: &str, base: Option<PathBuf>, cx: &mut gpui::Context<Self>) -> AnyElement {
        let (lines, blocks): (Vec<usize>, Vec<Block>) = parse_markdown_lines(text).into_iter().unzip();
        let _ = lines;
        let mut found = Vec::new();
        code_blocks(&blocks, &mut found);
        let code: Vec<CodeSpans> = found.into_iter().map(|(language, text)| self.highlight_code(language, text)).collect();
        let draw = Draw {
            theme: &self.theme,
            entity: cx.entity(),
            base,
            code: &code,
            next_code: Cell::new(0),
            next_text: Cell::new(0),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .text_size(px(UI_FONT_SIZE + 1.0))
            .children(blocks.iter().map(|block| render_block(block, &draw)))
            .into_any_element()
    }

    pub(crate) fn render_markdown_preview(&self, gi: usize, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let group = &self.groups[gi];
        let doc_id = group.tabs[group.active].doc;
        let parsed = self.markdown_doc(doc_id);
        let theme = &self.theme;
        let caret_block = self.sync_preview(gi, &parsed, cx);
        let draw = Draw {
            theme,
            entity: cx.entity(),
            base: self.doc_ref(doc_id).0.path.as_deref().and_then(|p| p.parent()).map(Path::to_path_buf),
            code: &parsed.code,
            next_code: Cell::new(0),
            next_text: Cell::new(0),
        };
        div()
            .id(("markdown-preview", group.id.0 as usize))
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&group.preview_scroll)
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .text_color(theme.text)
            .text_size(px(UI_FONT_SIZE + 1.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(24.))
                    // A definite width, so text wraps at it; `max_w` alone left lines unwrapped.
                    .w(px(820.))
                    .max_w_full()
                    .children(parsed.blocks.iter().enumerate().map(|(i, block)| {
                        let cells = group.preview_blocks.clone();
                        div()
                            .relative()
                            .w_full()
                            .flex_none()
                            .rounded(px(4.))
                            .when(i == caret_block, |b| b.bg(theme.line_highlight))
                            .child(render_block(block, &draw))
                            .child(
                                canvas(
                                    |bounds, _, _| bounds,
                                    move |bounds, _, _, _| {
                                        if let Some(slot) = cells.borrow_mut().get_mut(i) {
                                            *slot = bounds;
                                        }
                                    },
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                    })),
            )
    }

    /// Where the preview of group `gi` takes its position from: the editor of the same file, in
    /// this group (side by side) or another one (a preview panel). `None` when no editor shows it.
    fn preview_source(&self, gi: usize) -> Option<usize> {
        let group = &self.groups[gi];
        if group.preview == PreviewMode::Side {
            return Some(gi);
        }
        let doc = group.tabs[group.active].doc;
        self.groups.iter().enumerate().find_map(|(i, other)| {
            (i != gi && other.preview != PreviewMode::Only && other.tabs[other.active].doc == doc).then_some(i)
        })
    }

    /// Bring the preview of group `gi` and its editor together, and return the block holding the
    /// caret. Whichever of the two was scrolled since the last frame leads, and the other follows,
    /// by matching the editor's top line with the same place in the page. With no editor on
    /// screen the page keeps the caret's block in view.
    fn sync_preview(&self, gi: usize, parsed: &MarkdownDoc, cx: &mut gpui::Context<Self>) -> usize {
        let group = &self.groups[gi];
        let (document, _) = self.doc_ref(group.tabs[group.active].doc);
        let buffer = document.editor.buffer();
        let caret = document.editor.selections().last().map_or(0, |s| buffer.char_to_line(s.head));
        let block_at = |line: usize| parsed.lines.partition_point(|&start| start <= line).saturating_sub(1);
        let caret_block = block_at(caret);

        let handle = &group.preview_scroll;
        // Blocks as (distance from the first block, height), whatever the scroll position.
        let spans: Vec<(f32, f32)> = {
            let blocks = group.preview_blocks.borrow();
            blocks
                .first()
                .map(|first| blocks.iter().map(|b| (f32::from(b.top() - first.top()), f32::from(b.size.height))).collect())
                .unwrap_or_default()
        };
        group.preview_blocks.borrow_mut().resize(parsed.blocks.len(), Default::default());
        let line_count = buffer.line_count() as f32;
        let block_lines = |index: usize| {
            let start = parsed.lines.get(index).copied().unwrap_or(0) as f32;
            let end = parsed.lines.get(index + 1).map_or(line_count, |&l| l as f32);
            (start, end)
        };

        let revision = document.editor.revision();
        let previous = group.preview_sync.get();
        let mut next = PreviewSync { caret_block, revision, ..previous };
        let source = self.preview_source(gi);
        let measured = spans.len() == parsed.blocks.len() && !spans.is_empty();

        if let (Some(src), true) = (source, measured) {
            let editor_handle = self.groups[src].scroll.0.borrow().base_handle.clone();
            let source_wrap = self.group_wrap(src);
            // The editor scrolls in rows; the page is matched by source line.
            let lines_of = |offset: Pixels| {
                let row = f32::from(-offset / px(line_height())).max(0.0);
                source_wrap.as_ref().map_or(row, |w| w.line_at_row(row))
            };
            let top_line = lines_of(editor_handle.offset().y);
            let offset = f32::from(handle.offset().y);
            let editor_moved = (top_line - previous.top_line).abs() > 0.05 || revision != previous.revision;
            let page_moved = (offset - previous.offset).abs() > 1.0;
            // The ends match exactly: the last line of the file is the end of the page, though the
            // editor's top line stops well short of the last line.
            let editor_max = f32::from(editor_handle.max_offset().height);
            let page_max = f32::from(handle.max_offset().height);
            if editor_moved {
                let at_end = editor_max > 0.0 && f32::from(-editor_handle.offset().y) >= editor_max - 1.0;
                if at_end {
                    handle.set_offset(point(px(0.), px(-page_max)));
                } else {
                    // Line to pixel: the block holding the top line, and how far into it the line is.
                    let index = block_at(top_line as usize);
                    let (start, end) = block_lines(index);
                    let inside = if end > start { ((top_line - start) / (end - start)).clamp(0.0, 1.0) } else { 0.0 };
                    let (top, height) = spans[index];
                    handle.set_offset(point(px(0.), px(-(top + height * inside).min(page_max))));
                }
                next.top_line = top_line;
            } else if page_moved {
                if page_max > 0.0 && -offset >= page_max - 1.0 {
                    editor_handle.set_offset(point(px(0.), px(-editor_max)));
                } else {
                    // Pixel to line: the block under the top of the page, and how far into it.
                    let y = -offset;
                    let index = spans.partition_point(|&(top, _)| top <= y).saturating_sub(1);
                    let (top, height) = spans[index];
                    let inside = if height > 0.0 { ((y - top) / height).clamp(0.0, 1.0) } else { 0.0 };
                    let (start, end) = block_lines(index);
                    let line = start + (end - start) * inside;
                    let row = source_wrap.as_ref().map_or(line, |w| w.row_at_line(line));
                    editor_handle.set_offset(point(px(0.), px(-(row * line_height()).min(editor_max))));
                }
                // What the editor will show once it has clamped, not what was asked for.
                next.top_line = lines_of(editor_handle.offset().y);
            }
            next.offset = f32::from(handle.offset().y);
        } else if source.is_none() && caret_block != previous.caret_block {
            if let Some(&(top, _)) = spans.get(caret_block) {
                handle.set_offset(point(px(0.), px(-top)));
            }
        }
        if next == previous {
            return caret_block;
        }
        group.preview_sync.set(next);
        // The editor applies jumps of its own (to the caret, to the start) while it paints, and the
        // page is measured as it paints; look again on the next frame.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        caret_block
    }

    /// Show or hide the preview beside the active group's editor.
    pub(crate) fn toggle_markdown_preview(&mut self, cx: &mut gpui::Context<Self>) {
        self.set_preview(PreviewMode::Side, cx);
    }

    /// Show or hide the page in place of the active group's editor.
    pub(crate) fn toggle_markdown_full_preview(&mut self, cx: &mut gpui::Context<Self>) {
        self.set_preview(PreviewMode::Only, cx);
    }

    /// The page in a new group to the right, following the editor.
    pub(crate) fn open_markdown_preview_panel(&mut self, cx: &mut gpui::Context<Self>) {
        let group = &self.groups[self.active_group];
        if !crate::media_view::is_previewable(&self.doc_label(group.tabs[group.active].doc).0) {
            self.status = Some("Preview needs a Markdown or SVG file".to_owned());
            cx.notify();
            return;
        }
        self.groups[self.active_group].preview = PreviewMode::Off;
        self.split_active(SplitDir::Right, cx);
        self.groups[self.active_group].preview = PreviewMode::Only;
        cx.notify();
    }

    fn set_preview(&mut self, mode: PreviewMode, cx: &mut gpui::Context<Self>) {
        let group = &self.groups[self.active_group];
        let title = self.doc_label(group.tabs[group.active].doc).0;
        if !crate::media_view::is_previewable(&title) {
            self.status = Some("Preview needs a Markdown or SVG file".to_owned());
        } else {
            let group = &mut self.groups[self.active_group];
            group.preview = if group.preview == mode { PreviewMode::Off } else { mode };
            group.preview_sync.set(PreviewSync::default());
        }
        cx.notify();
    }

    /// Follow a link of the preview: a web address opens in the browser, a file path in the editor.
    fn follow_link(&mut self, target: LinkTarget, cx: &mut gpui::Context<Self>) {
        match target {
            LinkTarget::Web(url) => cx.open_url(&url),
            LinkTarget::File(path) if path.is_file() => self.open_path(&path, cx),
            LinkTarget::File(path) => {
                self.status = Some(format!("Cannot find {}", path.display()));
                cx.notify();
            }
            LinkTarget::None => {}
        }
    }
}

fn render_block(block: &Block, draw: &Draw) -> AnyElement {
    let theme = draw.theme;
    match block {
        Block::Heading { level, spans } => {
            let size = match level {
                1 => 30.0,
                2 => 23.0,
                3 => 19.0,
                4 => 16.0,
                _ => 14.0,
            };
            let heading = div()
                .mt(px(if *level <= 2 { 8. } else { 2. }))
                .text_size(px(size))
                .font_weight(FontWeight::BOLD)
                .child(styled(spans, draw, true));
            heading.into_any_element()
        }
        Block::Paragraph(spans) => {
            div().line_height(px(UI_FONT_SIZE * 1.9)).child(styled(spans, draw, false)).into_any_element()
        }
        Block::Code { text, .. } => {
            let index = draw.next_code.get();
            draw.next_code.set(index + 1);
            let colours = draw.code.get(index).map(Vec::as_slice).unwrap_or(&[]);
            let block = div()
                .p(px(12.))
                .rounded(px(4.))
                .bg(theme.widget_background)
                .border_1()
                .border_color(theme.border)
                .font_family(code_font())
                .text_size(px(UI_FONT_SIZE))
                .id(("markdown-code", index))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .flex()
                .flex_row()
                // The lines sit in a column as wide as the longest of them, so there is something to
                // scroll; a column that stretched to the block would hide the overflow.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .min_w_full()
                        .children(text.trim_end().split('\n').enumerate().map(|(i, line)| {
                            let spans = colours.get(i).map(Vec::as_slice).unwrap_or(&[]);
                            // An empty line still needs height.
                            div().whitespace_nowrap().h(px(UI_FONT_SIZE * 1.5)).flex_none().child(code_line(line, spans, theme))
                        })),
                )
                ;
            // A gesture that is mostly sideways belongs to the block: it must not also move the page.
            div()
                .w_full()
                .on_scroll_wheel(|event, _window, cx| {
                    let delta = event.delta.pixel_delta(px(line_height()));
                    if delta.x.abs() > delta.y.abs() {
                        cx.stop_propagation();
                    }
                })
                .child(block)
                .into_any_element()
        }
        Block::Quote(blocks) => div()
            .pl(px(12.))
            .border_l_4()
            .border_color(theme.border)
            .text_color(theme.gutter_text)
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(blocks.iter().map(|b| render_block(b, draw)))
            .into_any_element(),
        Block::List { start, items } => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(items.iter().enumerate().map(|(i, item)| render_item(*start, i, item, draw)))
            .into_any_element(),
        Block::Table { header, rows } => {
            let border = theme.border;
            let row = |cells: &Vec<Vec<Span>>, head: bool| {
                div().flex().flex_row().border_b_1().border_color(border).children(cells.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(10.))
                        .py(px(6.))
                        .when(head, |c| c.font_weight(FontWeight::BOLD))
                        .child(styled(cell, draw, head))
                }))
            };
            div()
                .border_1()
                .border_color(border)
                .child(row(header, true).bg(theme.widget_background))
                .children(rows.iter().map(|r| row(r, false)))
                .into_any_element()
        }
        Block::Image { src, alt } => render_image(src, alt, draw),
        Block::Rule => div().h(px(1.)).w_full().bg(theme.border).into_any_element(),
    }
}

/// A picture from a file beside the document. Pictures from the web are not fetched; their
/// description shows as a link instead.
fn render_image(src: &str, alt: &str, draw: &Draw) -> AnyElement {
    match link_target(src, draw.base.as_deref()) {
        LinkTarget::File(path) if path.is_file() => img(path).max_w(px(780.)).into_any_element(),
        _ => {
            let label = if alt.is_empty() { src.to_owned() } else { alt.to_owned() };
            let span = Span { text: format!("[image: {label}]"), link: Some(src.to_owned()), ..Span::default() };
            div().child(styled(&[span], draw, false)).into_any_element()
        }
    }
}

fn render_item(start: Option<u64>, index: usize, item: &ListItem, draw: &Draw) -> AnyElement {
    let theme = draw.theme;
    // The marker sits in a box as tall as a line of text, so it stays level with the first line.
    let marker = div()
        .flex_none()
        .w(px(24.))
        .h(px(UI_FONT_SIZE * 1.9))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.gutter_text);
    let marker = match (item.checked, start) {
        (Some(true), _) => marker.child(crate::icons::icon("checkbox-checked", 18., rgb(LINK_COLOR))),
        (Some(false), _) => marker.child(crate::icons::icon("checkbox", 18., theme.gutter_text)),
        (None, Some(n)) => marker.child(format!("{}.", n + index as u64)),
        (None, None) => marker.child(div().size(px(6.)).rounded_full().bg(theme.gutter_text)),
    };
    div()
        .flex()
        .flex_row()
        .gap(px(6.))
        .child(marker)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(item.blocks.iter().map(|b| render_block(b, draw))),
        )
        .into_any_element()
}

/// One line of a code block, coloured by its token spans (character columns).
pub(crate) fn code_line(line: &str, spans: &[LineSpan], theme: &Theme) -> StyledText {
    let offsets: Vec<usize> = line.char_indices().map(|(i, _)| i).chain([line.len()]).collect();
    let runs: Vec<(std::ops::Range<usize>, HighlightStyle)> = spans
        .iter()
        .filter_map(|span| {
            let start = *offsets.get(span.start)?;
            let end = *offsets.get(span.end.min(offsets.len() - 1))?;
            (start < end).then(|| (start..end, theme.highlight_style(span.style)))
        })
        .collect();
    StyledText::new(SharedString::from(line.to_owned())).with_highlights(runs)
}

/// Text of `spans` with each span's style. Links can be clicked.
fn styled(spans: &[Span], draw: &Draw, heading: bool) -> AnyElement {
    let theme = draw.theme;
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, LinkTarget)> = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let link: Hsla = rgb(LINK_COLOR).into();
        let style = HighlightStyle {
            color: span.link.as_ref().map(|_| link),
            font_weight: (span.bold && !heading).then_some(FontWeight::BOLD),
            font_style: span.italic.then_some(FontStyle::Italic),
            background_color: span.code.then(|| theme.widget_background.into()),
            underline: span.link.as_ref().map(|_| UnderlineStyle { thickness: px(1.), color: Some(link), wavy: false }),
            strikethrough: span.strike.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
            ..Default::default()
        };
        if style != HighlightStyle::default() {
            highlights.push((start..text.len(), style));
        }
        if let Some(url) = &span.link {
            links.push((start..text.len(), link_target(url, draw.base.as_deref())));
        }
    }
    let styled = StyledText::new(text).with_highlights(highlights);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let id = draw.next_text.get();
    draw.next_text.set(id + 1);
    let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    let entity = draw.entity.clone();
    InteractiveText::new(("markdown-text", id), styled)
        .on_click(ranges, move |index, _window, cx| {
            let target = targets[index].clone();
            entity.update(cx, |view, cx| view.follow_link(target, cx));
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_lead_to_the_web_or_to_files_beside_the_document() {
        let base = Path::new("/w/docs");
        assert!(matches!(link_target("https://x.io/a", Some(base)), LinkTarget::Web(_)));
        assert!(matches!(link_target("mailto:a@b.c", None), LinkTarget::Web(_)));
        match link_target("img/a.png#top", Some(base)) {
            LinkTarget::File(path) => assert_eq!(path, Path::new("/w/docs/img/a.png")),
            _ => panic!("expected a file"),
        }
        assert!(matches!(link_target("#section", Some(base)), LinkTarget::None));
        assert!(matches!(link_target("a.md", None), LinkTarget::None));
    }
}

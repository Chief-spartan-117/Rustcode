//! The hover popup for text that is Markdown (what language servers answer): headings, emphasis,
//! inline code, lists, quotes and code blocks drawn in the code font with their syntax colors. A
//! long answer scrolls inside the popup instead of being cut short.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, point, prelude::*, px, rgb, AnyElement, Bounds, FontStyle, FontWeight, HighlightStyle, Hsla, Pixels, Point,
    StrikethroughStyle, StyledText, UnderlineStyle, Window,
};
use rustcode_domain::{parse_markdown, Block, ListItem, Span};

use crate::editor_view::EditorView;
use crate::markdown_view::{code_blocks, code_line, CodeSpans, LINK_COLOR};
use crate::text_layout::code_font;
use crate::theme::Theme;

/// The popup's width. Text wraps at it, so it needs a definite width rather than a maximum.
const POPUP_WIDTH: f32 = 520.0;
/// The tallest the popup gets before it scrolls.
const POPUP_MAX_HEIGHT: f32 = 380.0;
const TEXT_SIZE: f32 = 12.5;

/// A hover answer, parsed and with its code blocks colored, kept while the popup is up.
pub(crate) struct HoverDoc {
    blocks: Vec<Block>,
    code: Vec<CodeSpans>,
}

/// Where the popup was drawn last, so that the pointer can travel from the word to it.
pub(crate) type PopupBounds = Rc<Cell<Bounds<Pixels>>>;

impl EditorView {
    /// Parse `text` and color its code blocks.
    pub(crate) fn hover_doc(&self, text: &str) -> Rc<HoverDoc> {
        let blocks = parse_markdown(text);
        let mut found = Vec::new();
        code_blocks(&blocks, &mut found);
        let code = found.into_iter().map(|(language, text)| self.highlight_code(language, text)).collect();
        Rc::new(HoverDoc { blocks, code })
    }

    /// The popup for a Markdown answer, with its corner at `position` (the pointer's, or just
    /// below the caret's, place). It opens upwards when there is more room above.
    pub(crate) fn render_markdown_hover(&self, doc: &HoverDoc, bounds: &PopupBounds, position: Point<Pixels>, window: &Window) -> AnyElement {
        let colors = &self.theme.widgets;
        let viewport = window.viewport_size();
        let left = position.x.min(viewport.width - px(POPUP_WIDTH + 24.)).max(px(4.));
        let below = viewport.height - position.y - px(12.);
        let above = position.y - px(crate::text_layout::line_height() + 12.);
        // Open upwards when the room below is small and there is more above.
        let upwards = below < px(220.) && above > below;
        let room = if upwards { above } else { below };
        let max_height = px(POPUP_MAX_HEIGHT).min(room).max(px(80.));
        let next_code = Cell::new(0usize);
        let body: Vec<AnyElement> = doc.blocks.iter().map(|block| render_block(block, doc, &next_code, &self.theme)).collect();
        let cell = bounds.clone();
        let popup = div()
            .id("hover-popup")
            .absolute()
            .left(left)
            .w(px(POPUP_WIDTH))
            .max_w_full()
            .max_h(max_height)
            .overflow_y_scroll()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(4.))
            .border_1()
            .border_color(colors.hover_border)
            .bg(colors.hover_background)
            .text_color(colors.hover_foreground)
            .text_size(px(TEXT_SIZE))
            .shadow_md()
            .occlude()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(body)
            .child(
                // Records where the popup is, for `hover_pointer_moved`.
                canvas(|bounds, _, _| bounds, move |bounds, _, _, _| cell.set(bounds)).absolute().top_0().left_0().size_full(),
            );
        if upwards {
            popup.bottom(viewport.height - position.y + px(crate::text_layout::line_height())).into_any_element()
        } else {
            popup.top(position.y).into_any_element()
        }
    }
}

impl EditorView {
    /// A panel of Markdown with its corner at `(left, top)`, as wide as `width` and no taller than
    /// `max_height`, scrolling beyond that. Used for the documentation beside a suggestion.
    pub(crate) fn render_markdown_panel(&self, doc: &HoverDoc, id: &'static str, left: Pixels, top: Pixels, width: f32, max_height: Pixels) -> AnyElement {
        let colors = &self.theme.widgets;
        let next_code = Cell::new(0usize);
        let body: Vec<AnyElement> = doc.blocks.iter().map(|block| render_block(block, doc, &next_code, &self.theme)).collect();
        div()
            .id(id)
            .absolute()
            .left(left)
            .top(top)
            .w(px(width))
            .max_h(max_height)
            .overflow_y_scroll()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(4.))
            .border_1()
            .border_color(colors.suggest_border)
            .bg(colors.suggest_background)
            .text_color(colors.suggest_foreground)
            .text_size(px(TEXT_SIZE))
            .shadow_md()
            .occlude()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(body)
            .into_any_element()
    }
}

/// Whether the pointer is on the popup, or in the strip between a word and a popup below it, so
/// that moving toward the popup does not take it down.
pub(crate) fn pointer_on_popup(bounds: Bounds<Pixels>, pointer: Point<Pixels>) -> bool {
    if bounds.size.width <= px(0.) {
        return false;
    }
    // The popup sits one line below the word, so this strip is the gap between them: the word's
    // own line is outside it, and moving sideways along the line still leaves the word.
    let reach = px(crate::text_layout::line_height() * 0.75);
    let bridge = Bounds::new(point(bounds.origin.x, bounds.origin.y - reach), gpui::size(bounds.size.width, bounds.size.height + reach));
    bridge.contains(&pointer)
}

/// Text with the style of each span.
fn spans_text(spans: &[Span], theme: &Theme, bold: bool) -> AnyElement {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let link: Hsla = rgb(LINK_COLOR).into();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let style = HighlightStyle {
            color: span.link.as_ref().map(|_| link),
            font_weight: (span.bold && !bold).then_some(FontWeight::BOLD),
            font_style: span.italic.then_some(FontStyle::Italic),
            background_color: span.code.then(|| theme.background.into()),
            underline: span.link.as_ref().map(|_| UnderlineStyle { thickness: px(1.), color: Some(link), wavy: false }),
            strikethrough: span.strike.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
            ..Default::default()
        };
        if style != HighlightStyle::default() {
            highlights.push((start..text.len(), style));
        }
    }
    StyledText::new(text).with_highlights(highlights).into_any_element()
}

fn render_block(block: &Block, doc: &HoverDoc, next_code: &Cell<usize>, theme: &Theme) -> AnyElement {
    match block {
        Block::Heading { level, spans } => {
            div().text_size(px(if *level <= 2 { TEXT_SIZE + 2.5 } else { TEXT_SIZE + 1. })).font_weight(FontWeight::BOLD).child(spans_text(spans, theme, true)).into_any_element()
        }
        Block::Paragraph(spans) => div().line_height(px(TEXT_SIZE * 1.5)).child(spans_text(spans, theme, false)).into_any_element(),
        Block::Code { text, .. } => {
            let index = next_code.get();
            next_code.set(index + 1);
            let colours = doc.code.get(index).map(Vec::as_slice).unwrap_or(&[]);
            div()
                .px(px(8.))
                .py(px(5.))
                .rounded(px(3.))
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .font_family(code_font())
                .flex()
                .flex_col()
                // Long signatures wrap at the popup's edge.
                .children(text.trim_end().split('\n').enumerate().map(|(i, line)| {
                    let spans = colours.get(i).map(Vec::as_slice).unwrap_or(&[]);
                    div().min_h(px(TEXT_SIZE * 1.4)).child(code_line(line, spans, theme))
                }))
                .into_any_element()
        }
        Block::Quote(blocks) => div()
            .pl(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .text_color(theme.gutter_text)
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(blocks.iter().map(|b| render_block(b, doc, next_code, theme)))
            .into_any_element(),
        Block::List { start, items } => div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(items.iter().enumerate().map(|(i, item)| render_item(*start, i, item, doc, next_code, theme)))
            .into_any_element(),
        Block::Table { header, rows } => {
            let line = |cells: &Vec<Vec<Span>>| cells.iter().map(|c| c.iter().map(|s| s.text.as_str()).collect::<String>()).collect::<Vec<_>>().join("  |  ");
            div()
                .flex()
                .flex_col()
                .child(div().font_weight(FontWeight::BOLD).child(line(header)))
                .children(rows.iter().map(|r| div().child(line(r))))
                .into_any_element()
        }
        Block::Image { alt, src } => div().text_color(theme.gutter_text).child(if alt.is_empty() { src.clone() } else { alt.clone() }).into_any_element(),
        Block::Rule => div().h(px(1.)).w_full().bg(theme.border).into_any_element(),
    }
}

fn render_item(start: Option<u64>, index: usize, item: &ListItem, doc: &HoverDoc, next_code: &Cell<usize>, theme: &Theme) -> AnyElement {
    let marker = match (item.checked, start) {
        (Some(true), _) => "\u{2611}".to_owned(),
        (Some(false), _) => "\u{2610}".to_owned(),
        (None, Some(n)) => format!("{}.", n + index as u64),
        (None, None) => "\u{2022}".to_owned(),
    };
    div()
        .flex()
        .flex_row()
        .gap(px(6.))
        .child(div().flex_none().w(px(18.)).text_color(theme.gutter_text).child(marker))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).children(item.blocks.iter().map(|b| render_block(b, doc, next_code, theme))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_may_travel_from_the_word_to_the_popup_but_not_far_beside_it() {
        let popup = Bounds::new(point(px(100.), px(200.)), gpui::size(px(300.), px(120.)));
        assert!(pointer_on_popup(popup, point(px(150.), px(250.))), "on the popup");
        let lh = crate::text_layout::line_height();
        assert!(pointer_on_popup(popup, point(px(150.), px(200. - lh * 0.5))), "in the strip above it");
        assert!(!pointer_on_popup(popup, point(px(150.), px(200. - lh))), "on the line of the word");
        assert!(!pointer_on_popup(popup, point(px(150.), px(100.))), "well above");
        assert!(!pointer_on_popup(popup, point(px(50.), px(250.))), "beside it");
        // A popup that was never drawn is nowhere.
        assert!(!pointer_on_popup(Bounds::default(), point(px(0.), px(0.))));
    }
}

//! Colors written in the text: a square in the color before each, and a picker that opens when a
//! square is clicked (or from the command palette): the color as sliders, and the ways the language
//! can write it (`#rrggbb`, `rgb()`, `hsl()`).

use std::cell::Cell;

use gpui::{div, linear_color_stop, linear_gradient, point, prelude::*, px, Context, MouseButton, MouseDownEvent, Pixels, Point, Rgba, Window};
use rustcode_application::{ColorEntry, ColorPresentationEntry, LspRequest, TextEdit};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;
use crate::intelligence::Pending;
use crate::text_layout::HintText;

/// The popup's width, and where its sliders are inside it.
const WIDTH: f32 = 268.0;
const PADDING: f32 = 10.0;
const LABEL: f32 = 22.0;
const BAR: f32 = 188.0;
/// What stands before a color: a square, and a space to keep it from the text.
const SWATCH: &str = "\u{25a0} ";

/// The color being picked: which one it is, and what the sliders say.
pub(crate) struct ColorPicker {
    pub doc: DocumentId,
    pub revision: u64,
    /// Where the color is written, as lines and char columns.
    pub range: (usize, usize, usize, usize),
    pub color: [u8; 4],
    /// The ways to write the color now, as the provider gave them.
    pub presentations: Vec<ColorPresentationEntry>,
    /// The channel (0 red, 1 green, 2 blue, 3 alpha) a drag is on.
    pub dragging: Option<usize>,
    /// Where the popup was drawn last, which the drag measures from.
    pub origin: Cell<Point<Pixels>>,
}

fn rgba(color: [u8; 4]) -> Rgba {
    Rgba { r: f32::from(color[0]) / 255.0, g: f32::from(color[1]) / 255.0, b: f32::from(color[2]) / 255.0, a: f32::from(color[3]) / 255.0 }
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
pub(crate) fn hex(color: [u8; 4]) -> String {
    if color[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", color[0], color[1], color[2], color[3])
    }
}

impl EditorView {
    /// Whether squares are drawn in the lines of `doc`: not when lines wrap, since a square would
    /// move where they break.
    fn colors_fit(&self, doc: DocumentId) -> bool {
        !self.wraps(&self.doc_ref(doc).0.title)
    }

    /// The squares to draw in line `line` of `doc`.
    pub(crate) fn swatches_in_line(&self, doc: DocumentId, line: usize) -> Vec<HintText> {
        let Some((revision, colors)) = self.features.colors.get(&doc) else { return Vec::new() };
        if *revision != self.doc_ref(doc).0.editor.revision() || !self.colors_fit(doc) {
            return Vec::new();
        }
        colors
            .iter()
            .filter(|c| c.line == line)
            .map(|c| HintText { col: c.column, text: SWATCH.to_owned(), color: Some(rgba(c.color)) })
            .collect()
    }

    /// Ask for the colors of the file once it has rested after a change.
    pub(crate) fn ask_colors(&mut self, doc: DocumentId, revision: u64, rested: bool, cx: &mut Context<Self>) {
        if !rested || self.features.colors_asked.get(&doc) == Some(&revision) || self.document.path.is_none() {
            return;
        }
        self.features.colors_asked.insert(doc, revision);
        self.ask_quiet(Pending::Colors { doc, revision }, |path| LspRequest::DocumentColors { path }, cx);
    }

    pub(crate) fn colors_ready(&mut self, doc: DocumentId, revision: u64, colors: Vec<ColorEntry>, cx: &mut Context<Self>) {
        if doc != self.document.id || revision != self.document.editor.revision() {
            return;
        }
        self.features.colors.insert(doc, (revision, colors));
        cx.notify();
    }

    /// The color that starts at `line`, `column` of the open file, or holds the caret.
    fn color_at(&self, line: usize, column: usize) -> Option<ColorEntry> {
        let (revision, colors) = self.features.colors.get(&self.document.id)?;
        if *revision != self.document.editor.revision() {
            return None;
        }
        colors.iter().find(|c| c.line == line && c.column <= column && (column <= c.end_column || c.end_line > line)).cloned()
    }

    /// The color a click at `x` of line `line` is on the square of, if any.
    pub(crate) fn swatch_color_at(&self, line: usize, x: Pixels, window: &Window) -> Option<ColorEntry> {
        let raw = self.document.editor.buffer().line(line).unwrap_or_default();
        let layout = self.layout_line(self.document.id, line, &raw, window);
        let column = layout.swatch_at(x)?;
        self.color_at(line, column).filter(|c| c.column == column)
    }

    /// Open the picker for `entry`, asking for the ways to write it.
    pub(crate) fn open_color_picker(&mut self, entry: ColorEntry, cx: &mut Context<Self>) {
        self.color_picker = Some(ColorPicker {
            doc: self.document.id,
            revision: self.document.editor.revision(),
            range: (entry.line, entry.column, entry.end_line, entry.end_column),
            color: entry.color,
            presentations: Vec::new(),
            dragging: None,
            origin: Cell::new(point(px(0.), px(0.))),
        });
        self.ask_color_presentations(cx);
        cx.notify();
    }

    /// `Edit: Pick Color` (the command): the color at the caret.
    pub(crate) fn pick_color_at_caret(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        match self.color_at(line, column) {
            Some(entry) => self.open_color_picker(entry, cx),
            None => self.set_status("no color at the caret"),
        }
        cx.notify();
    }

    fn ask_color_presentations(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.color_picker.as_ref() else { return };
        let (doc, revision, range, color) = (picker.doc, picker.revision, picker.range, picker.color);
        self.ask_quiet(Pending::ColorPresentations { doc, revision }, |path| LspRequest::ColorPresentation { path, range, color }, cx);
    }

    pub(crate) fn color_presentations_ready(&mut self, doc: DocumentId, revision: u64, forms: Vec<ColorPresentationEntry>, cx: &mut Context<Self>) {
        if let Some(picker) = self.color_picker.as_mut().filter(|p| p.doc == doc && p.revision == revision) {
            picker.presentations = forms;
            cx.notify();
        }
    }

    pub(crate) fn close_color_picker(&mut self, cx: &mut Context<Self>) {
        if self.color_picker.take().is_some() {
            cx.notify();
        }
    }

    /// Escape takes the picker down. Returns whether the key was used.
    pub(crate) fn color_picker_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.color_picker.is_some() && key == "escape" {
            self.close_color_picker(cx);
            return true;
        }
        false
    }

    /// Set channel `channel` from a pointer at window x `x`.
    fn set_channel(&mut self, channel: usize, x: Pixels, cx: &mut Context<Self>) {
        let Some(picker) = self.color_picker.as_mut() else { return };
        let left = picker.origin.get().x + px(PADDING + LABEL);
        let fraction = f32::from((x - left) / px(BAR)).clamp(0.0, 1.0);
        picker.color[channel] = (fraction * 255.0).round() as u8;
        cx.notify();
    }

    /// The pointer moved while a slider is held.
    pub(crate) fn color_picker_drag(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if let Some(channel) = self.color_picker.as_ref().and_then(|p| p.dragging) {
            self.set_channel(channel, x, cx);
        }
    }

    /// The button came up: a held slider is let go, and the ways to write the new color are asked for.
    pub(crate) fn color_picker_release(&mut self, cx: &mut Context<Self>) {
        if self.color_picker.as_mut().and_then(|p| p.dragging.take()).is_some() {
            self.ask_color_presentations(cx);
        }
    }

    /// Write the color in the text as way number `index` says, and close the picker.
    pub(crate) fn apply_color_presentation(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(picker) = self.color_picker.take() else { return };
        let Some(form) = picker.presentations.get(index).cloned() else { return };
        if picker.doc != self.document.id || picker.revision != self.document.editor.revision() {
            self.set_status("the text changed; pick the color again");
            cx.notify();
            return;
        }
        let (line, column, end_line, end_column) = picker.range;
        let edit = form.edit.unwrap_or(TextEdit { line, column, end_line, end_column, new_text: form.label });
        let ranges = crate::language_features::edit_ranges(self.document.editor.buffer(), &[edit]);
        self.edit_document(cx, move |e| e.replace_ranges(ranges));
        cx.notify();
    }

    pub(crate) fn render_color_picker(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let picker = self.color_picker.as_ref()?;
        if picker.doc != self.document.id {
            return None;
        }
        let theme = &self.theme;
        let (line, column, ..) = picker.range;
        let at = self.below_caret(line, column, window);
        let viewport = window.viewport_size();
        let left = at.x.min(viewport.width - px(WIDTH + 8.)).max(px(4.));
        let height = px(60. + 4. * 24. + 16. + picker.presentations.len().min(6) as f32 * 22.);
        let top = if at.y + height > viewport.height - px(30.) { (at.y - px(crate::text_layout::line_height()) - height).max(px(4.)) } else { at.y };
        picker.origin.set(point(left, top));
        let color = rgba(picker.color);
        let solid = |c: [u8; 4]| Rgba { a: 1.0, ..rgba(c) };
        let channels = ["R", "G", "B", "A"];
        let sliders = channels.iter().enumerate().map(|(channel, name)| {
            let (mut from, mut to) = (picker.color, picker.color);
            from[channel] = 0;
            to[channel] = 255;
            let (start, end) = if channel == 3 { (Rgba { a: 0.0, ..solid(picker.color) }, solid(picker.color)) } else { (solid(from), solid(to)) };
            let value = picker.color[channel];
            let thumb = f32::from(value) / 255.0 * BAR;
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(24.))
                .child(div().w(px(LABEL)).flex_none().text_color(theme.gutter_text).child(*name))
                .child(
                    div()
                        .id(("color-slider", channel))
                        .relative()
                        .w(px(BAR))
                        .h(px(14.))
                        .flex_none()
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .bg(linear_gradient(90., linear_color_stop(start, 0.), linear_color_stop(end, 1.)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                if let Some(picker) = this.color_picker.as_mut() {
                                    picker.dragging = Some(channel);
                                }
                                this.set_channel(channel, event.position.x, cx);
                            }),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(-3.))
                                .left(px((thumb - 3.).max(0.)))
                                .w(px(6.))
                                .h(px(18.))
                                .rounded(px(2.))
                                .border_1()
                                .border_color(gpui::white())
                                .bg(theme.background),
                        ),
                )
                .child(div().w(px(34.)).flex_none().pl(px(6.)).text_color(theme.text).child(value.to_string()))
        });
        let forms = picker.presentations.iter().take(6).enumerate().map(|(index, form)| {
            div()
                .id(("color-form", index))
                .h(px(22.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .cursor_pointer()
                .font_family(crate::text_layout::ui_font())
                .hover(|style| style.bg(theme.lists.hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.apply_color_presentation(index, cx)
                    }),
                )
                .child(form.label.clone())
        });
        Some(
            div()
                .id("color-picker")
                .absolute()
                .left(left)
                .top(top)
                .w(px(WIDTH))
                .p(px(PADDING))
                .flex()
                .flex_col()
                .gap(px(4.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme.widgets.hover_border)
                .bg(theme.widgets.hover_background)
                .text_color(theme.widgets.hover_foreground)
                .text_size(px(12.))
                .shadow_md()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()))
                .child(
                    div()
                        .h(px(34.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme.border)
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .justify_end()
                        .bg(color)
                        // Dark or light text, whichever reads against the color.
                        .text_color(if f32::from(picker.color[0]) * 0.3 + f32::from(picker.color[1]) * 0.6 + f32::from(picker.color[2]) * 0.1 > 140. {
                            gpui::black()
                        } else {
                            gpui::white()
                        })
                        .font_family(crate::text_layout::ui_font())
                        .child(hex(picker.color)),
                )
                .children(sliders)
                .children(forms),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_written_as_hex_with_alpha_only_when_not_opaque() {
        assert_eq!(hex([255, 128, 0, 255]), "#ff8000");
        assert_eq!(hex([0, 0, 0, 0]), "#00000000");
        assert_eq!(hex([1, 2, 3, 128]), "#01020380");
    }
}

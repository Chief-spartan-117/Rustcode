//! The minimap: the whole file drawn tiny along the right edge, in its syntax colors, with a box
//! over the part that shows in the editor. Click or drag in it to scroll.

use gpui::{canvas, div, point, prelude::*, px, size, AnyElement, Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, ScrollHandle};

use crate::editor_view::EditorView;

/// Width of the column.
pub(crate) const MINIMAP_WIDTH: f32 = 96.0;
/// Height one line takes, and width one character takes.
const PITCH: f32 = 3.0;
const CHAR_WIDTH: f32 = 1.0;
const PADDING: f32 = 6.0;
/// Files with more lines than this are not drawn: the cost grows with the file, not the view.
/// How much of its color the code of the minimap keeps.
const DIM: f32 = 0.45;
const MAX_LINES: usize = 20_000;

/// Where the minimap is in relation to the editor: all heights in pixels.
struct Geometry {
    viewport: f32,
    content: f32,
    max: f32,
    offset: f32,
    /// Height of the whole file in the minimap.
    total: f32,
    /// How far the minimap itself is scrolled (it only scrolls when the file is taller than it).
    scroll: f32,
}

impl Geometry {
    fn of(handle: &ScrollHandle, lines: usize) -> Option<Self> {
        let viewport = f32::from(handle.bounds().size.height);
        if viewport <= 0.0 {
            return None;
        }
        let max = f32::from(handle.max_offset().height).max(0.0);
        let content = viewport + max;
        let offset = (-f32::from(handle.offset().y)).clamp(0.0, max);
        let total = lines as f32 * PITCH;
        let scroll = if total > viewport && max > 0.0 { offset / max * (total - viewport) } else { 0.0 };
        Some(Self { viewport, content, max, offset, total, scroll })
    }
}

impl EditorView {
    /// The minimap of group `gi`, or nothing when it is off or the file is not text to draw.
    pub(crate) fn render_minimap(&self, gi: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.minimap_on() {
            return None;
        }
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let handle = group.scroll.0.borrow().base_handle.clone();
        let (document, highlighter) = self.doc_ref(doc);
        let buffer = document.editor.buffer();
        let lines = buffer.line_count().max(1);
        if lines > MAX_LINES {
            return None;
        }
        let theme = &self.theme;
        let geometry = Geometry::of(&handle, lines)?;
        let first = (geometry.scroll / PITCH).floor() as usize;
        let last = (((geometry.scroll + geometry.viewport) / PITCH).ceil() as usize).min(lines);
        // The code is dimmed, so the box over the visible part stands out.
        let dim = |mut color: Hsla| {
            color.a *= DIM;
            color
        };
        let default: Hsla = dim(Hsla::from(theme.text));
        let columns = ((MINIMAP_WIDTH - PADDING) / CHAR_WIDTH) as usize;

        // One rectangle per run of text of one color: x, width, y, color.
        let mut rects: Vec<(f32, f32, f32, Hsla)> = Vec::new();
        for line in first..last {
            let text = buffer.line(line).unwrap_or_default();
            let spans = highlighter.map_or(&[][..], |h| h.line(line));
            let y = line as f32 * PITCH - geometry.scroll;
            let mut run: Option<(usize, Hsla)> = None;
            let mut col = 0usize;
            let mut flush = |run: &mut Option<(usize, Hsla)>, end: usize| {
                if let Some((start, color)) = run.take() {
                    rects.push((PADDING / 2.0 + start as f32 * CHAR_WIDTH, (end - start) as f32 * CHAR_WIDTH, y, color));
                }
            };
            for (index, ch) in text.trim_end_matches(['\n', '\r']).chars().enumerate() {
                if col >= columns {
                    break;
                }
                if ch.is_whitespace() {
                    flush(&mut run, col);
                    col += if ch == '\t' { 4 } else { 1 };
                    continue;
                }
                let color = spans
                    .iter()
                    .find(|s| s.start <= index && index < s.end)
                    .and_then(|s| theme.highlight_style(s.style).color)
                    .map_or(default, dim);
                match run {
                    Some((_, current)) if current == color => {}
                    _ => {
                        flush(&mut run, col);
                        run = Some((col, color));
                    }
                }
                col += 1;
            }
            flush(&mut run, col.min(columns));
        }

        let slider_top = geometry.offset / geometry.content * geometry.total - geometry.scroll;
        let slider_height = (geometry.viewport / geometry.content * geometry.total).min(geometry.viewport).max(8.0);
        let slider = theme.scrollbar.slider;
        let jump = {
            let handle = handle.clone();
            move |y: f32, this: &mut Self, cx: &mut Context<Self>| {
                let Some(g) = Geometry::of(&handle, lines) else { return };
                if g.max <= 0.0 {
                    return;
                }
                // The editor position that the pressed place of the minimap shows, centered.
                let target = (y + g.scroll) / g.total.max(1.0) * g.content - g.viewport / 2.0;
                handle.set_offset(point(handle.offset().x, px(-target.clamp(0.0, g.max))));
                let _ = this;
                cx.notify();
            }
        };
        let (on_down, on_move) = (jump.clone(), jump);
        let origin = handle.clone();
        Some(
            div()
                .id(("minimap", gi))
                .relative()
                .flex_none()
                .w(px(MINIMAP_WIDTH))
                .h_full()
                .overflow_hidden()
                .bg(theme.background)
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        on_down(f32::from(event.position.y - origin.bounds().origin.y), this, cx);
                    }),
                )
                .on_mouse_move({
                    let origin = handle.clone();
                    cx.listener(move |this, event: &MouseMoveEvent, _w, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            on_move(f32::from(event.position.y - origin.bounds().origin.y), this, cx);
                        }
                    })
                })
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds: Bounds<gpui::Pixels>, _, window, _| {
                            for (x, width, y, color) in &rects {
                                let at = point(bounds.origin.x + px(*x), bounds.origin.y + px(*y));
                                window.paint_quad(gpui::fill(Bounds::new(at, size(px(*width), px(PITCH - 1.0))), *color));
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(div().absolute().left_0().w_full().top(px(slider_top)).h(px(slider_height)).bg(slider))
                .into_any_element(),
        )
    }
}

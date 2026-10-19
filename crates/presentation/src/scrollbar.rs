//! A vertical scrollbar for any scrollable list: a thumb that shows where the view is and can
//! be dragged, or clicked beside to jump. Colors are `scrollbarSlider.*`.

use gpui::{div, point, prelude::*, px, Context, MouseButton, MouseDownEvent, ScrollHandle};

use crate::editor_view::EditorView;

const BAR_WIDTH: f32 = 12.0;
const THUMB_WIDTH: f32 = 8.0;
const MIN_THUMB: f32 = 24.0;

/// A thumb being dragged: which list, and how far below the thumb's top the pointer took hold.
pub(crate) struct ScrollDrag {
    key: usize,
    handle: ScrollHandle,
    grab: f32,
}

/// Where the thumb is, as `(top, height, travel, max_scroll)` in pixels, or `None` when the
/// content fits and there is nothing to scroll.
fn thumb_geometry(viewport: f32, max_scroll: f32, offset: f32) -> Option<(f32, f32, f32, f32)> {
    if viewport <= 0.0 || max_scroll <= 0.5 {
        return None;
    }
    let content = viewport + max_scroll;
    let height = (viewport * viewport / content).clamp(MIN_THUMB.min(viewport), viewport);
    let travel = viewport - height;
    let fraction = (-offset / max_scroll).clamp(0.0, 1.0);
    Some((fraction * travel, height, travel, max_scroll))
}

/// The scroll position for a thumb whose top is at `top` within a track.
fn scroll_for_thumb_top(top: f32, travel: f32, max_scroll: f32) -> f32 {
    if travel <= 0.0 {
        return 0.0;
    }
    (top / travel).clamp(0.0, 1.0) * max_scroll
}

impl EditorView {
    /// `inner` (a scrollable list that tracks `handle`) with a scrollbar on its right edge.
    pub(crate) fn scroll_area(
        &self,
        key: usize,
        handle: &ScrollHandle,
        inner: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(inner)
            .children(self.scrollbar(key, handle, cx))
    }

    /// The scrollbar alone, to lay over a list that already is `relative`.
    pub(crate) fn scrollbar(&self, key: usize, handle: &ScrollHandle, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let bounds = handle.bounds();
        let (top, height, _, _) = thumb_geometry(
            f32::from(bounds.size.height),
            f32::from(handle.max_offset().height),
            f32::from(handle.offset().y),
        )?;
        let colors = &self.theme.scrollbar;
        let dragging = self.scroll_drag.as_ref().is_some_and(|d| d.key == key);
        let (slider, hover) = (if dragging { colors.slider_active } else { colors.slider }, colors.slider_hover);
        let held = handle.clone();
        Some(
            div()
                .id(("scrollbar", key))
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(px(BAR_WIDTH))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.grab_scrollbar(key, &held, f32::from(event.position.y), cx);
                    }),
                )
                // The bar hides the window's own handlers beneath it, so it follows the drag too.
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _w, cx| {
                    if this.scroll_drag.is_some() {
                        if event.pressed_button == Some(MouseButton::Left) {
                            this.drag_scrollbar(f32::from(event.position.y), cx);
                        } else {
                            this.scroll_drag = None;
                        }
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &gpui::MouseUpEvent, _w, cx| {
                        this.scroll_drag = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(top))
                        .left(px((BAR_WIDTH - THUMB_WIDTH) / 2.0))
                        .w(px(THUMB_WIDTH))
                        .h(px(height))
                        .rounded(px(THUMB_WIDTH / 2.0))
                        .bg(slider)
                        .hover(move |style| style.bg(hover)),
                ),
        )
    }

    /// A press on the bar at window height `y`: hold the thumb where it is, or move it there.
    fn grab_scrollbar(&mut self, key: usize, handle: &ScrollHandle, y: f32, cx: &mut Context<Self>) {
        let bounds = handle.bounds();
        let Some((top, height, _, _)) = thumb_geometry(
            f32::from(bounds.size.height),
            f32::from(handle.max_offset().height),
            f32::from(handle.offset().y),
        ) else {
            return;
        };
        let in_track = y - f32::from(bounds.origin.y);
        // On the thumb: keep the grip. Beside it: the thumb centers on the pointer.
        let grab = if (top..=top + height).contains(&in_track) { in_track - top } else { height / 2.0 };
        self.scroll_drag = Some(ScrollDrag { key, handle: handle.clone(), grab });
        self.drag_scrollbar(y, cx);
    }

    /// The pointer moved to window height `y` while the thumb is held.
    pub(crate) fn drag_scrollbar(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = &self.scroll_drag else { return };
        let handle = drag.handle.clone();
        let bounds = handle.bounds();
        let max = f32::from(handle.max_offset().height);
        let Some((_, _, travel, _)) = thumb_geometry(f32::from(bounds.size.height), max, f32::from(handle.offset().y)) else {
            return;
        };
        let top = y - f32::from(bounds.origin.y) - drag.grab;
        let scroll = scroll_for_thumb_top(top, travel, max);
        handle.set_offset(point(handle.offset().x, px(-scroll)));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_scroll_means_no_thumb() {
        assert_eq!(thumb_geometry(400.0, 0.0, 0.0), None);
        assert_eq!(thumb_geometry(0.0, 100.0, 0.0), None);
    }

    #[test]
    fn the_thumb_is_the_visible_share_of_the_content() {
        // 400 visible of 1000 total: the thumb is 40% of the track.
        let (top, height, travel, max) = thumb_geometry(400.0, 600.0, 0.0).unwrap();
        assert_eq!((top, max), (0.0, 600.0));
        assert!((height - 160.0).abs() < 0.01, "{height}");
        assert!((travel - 240.0).abs() < 0.01);
        // Scrolled halfway down: the thumb is halfway along its travel.
        let (top, ..) = thumb_geometry(400.0, 600.0, -300.0).unwrap();
        assert!((top - 120.0).abs() < 0.01);
        // At the end it rests on the bottom of the track.
        let (top, height, ..) = thumb_geometry(400.0, 600.0, -600.0).unwrap();
        assert!((top + height - 400.0).abs() < 0.01);
    }

    #[test]
    fn a_huge_list_still_has_a_thumb_you_can_grab() {
        let (_, height, ..) = thumb_geometry(400.0, 10_000_000.0, 0.0).unwrap();
        assert_eq!(height, MIN_THUMB);
    }

    #[test]
    fn dragging_maps_the_thumb_back_to_a_scroll_position() {
        let (_, _, travel, max) = thumb_geometry(400.0, 600.0, 0.0).unwrap();
        assert_eq!(scroll_for_thumb_top(0.0, travel, max), 0.0);
        assert!((scroll_for_thumb_top(120.0, travel, max) - 300.0).abs() < 0.01);
        // Beyond either end is held at the end.
        assert_eq!(scroll_for_thumb_top(-50.0, travel, max), 0.0);
        assert!((scroll_for_thumb_top(10_000.0, travel, max) - 600.0).abs() < 0.01);
    }
}

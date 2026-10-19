//! Find/replace widget rendering.

use gpui::{
    canvas, div, prelude::*, px, Context, Div, MouseButton, MouseDownEvent, Pixels, Stateful,
    Window,
};
use rustcode_domain::Editor;

use crate::editor_view::{EditorView, Focus};
use crate::text_layout::{LineLayout, line_height};

pub(crate) const INPUT_WIDTH: f32 = 280.0;
pub(crate) const INPUT_PADDING: f32 = 6.0;

/// Horizontal scroll that keeps the caret visible in an input.
pub(crate) fn input_scroll(layout: &LineLayout, caret_col: usize, width: f32) -> Pixels {
    let visible = px(width - 2.0 * INPUT_PADDING - 2.0);
    (layout.x_for_col(caret_col) - visible).max(px(0.))
}

impl EditorView {
    pub(crate) fn render_find_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let options = self.find.options;
        let status_color = if self.find.error.is_some() {
            self.theme.markers.error
        } else {
            self.theme.gutter_text
        };

        let query_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .child(self.input(
                "find-input",
                &self.find.query,
                self.focus == Focus::Find,
                Focus::Find,
                0,
                INPUT_WIDTH,
                window,
                cx,
            ))
            .child(self.button("find-case", "Aa", options.case_sensitive, cx, |this, cx| {
                this.toggle_option(|o| o.case_sensitive = !o.case_sensitive, cx)
            }))
            .child(self.button("find-word", "ab", options.whole_word, cx, |this, cx| {
                this.toggle_option(|o| o.whole_word = !o.whole_word, cx)
            }))
            .child(self.button("find-regex", ".*", options.regex, cx, |this, cx| {
                this.toggle_option(|o| o.regex = !o.regex, cx)
            }))
            .child(
                div()
                    .min_w(px(110.))
                    .px(px(6.))
                    .text_color(status_color)
                    .child(self.find.status()),
            )
            .child(self.icon_button("find-prev", "chevron-up", false, cx, |this, cx| {
                this.find_step(false, cx)
            }))
            .child(self.icon_button("find-next", "chevron-down", false, cx, |this, cx| {
                this.find_step(true, cx)
            }))
            .child(self.icon_button("find-close", "close", false, cx, |this, cx| {
                this.close_find(cx)
            }));

        let replace_row = self.find.show_replace.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(self.input(
                    "replace-input",
                    &self.find.replacement,
                    self.focus == Focus::Replace,
                    Focus::Replace,
                    1,
                    INPUT_WIDTH,
                    window,
                    cx,
                ))
                .child(self.button("replace-one", "Replace", false, cx, |this, cx| {
                    this.replace_current(cx)
                }))
                .child(self.button("replace-all", "All", false, cx, |this, cx| {
                    this.replace_all(cx)
                }))
        });

        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(8.))
            .py(px(4.))
            .bg(self.theme.widget_background)
            .text_color(self.theme.text)
            .child(query_row)
            .children(replace_row)
    }

    /// Like [`Self::button`], with an interface icon in place of text.
    pub(crate) fn icon_button(
        &self,
        id: &'static str,
        icon: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let color = self.theme.text;
        self.button(id, "", active, cx, on_click)
            .w(px(line_height() + 2.))
            .justify_center()
            .child(crate::icons::icon(icon, 14., color))
    }

    pub(crate) fn button(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let hover = if active { self.theme.button.hover } else { self.theme.lists.hover };
        let mut button = div()
            .id(id)
            .h(px(line_height()))
            .px(px(6.))
            .flex()
            .items_center()
            .rounded(px(3.))
            .text_color(if active { self.theme.text } else { self.theme.gutter_text })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    // Buttons sit inside clickable rows; the row must not also react.
                    cx.stop_propagation();
                    on_click(this, cx)
                }),
            )
            .hover(move |style| style.bg(hover));
        if active {
            button = button.bg(self.theme.input.option_active_background).text_color(self.theme.button.foreground);
            if let Some(border) = self.theme.input.option_active_border {
                button = button.border_1().border_color(border);
            }
        }
        button.child(label)
    }

    /// Single-line text input drawn from a domain `Editor`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn input(
        &self,
        id: &'static str,
        editor: &Editor,
        active: bool,
        focus: Focus,
        slot: usize,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (family, font_size) = focus.input_font();
        let layout = LineLayout::shape(&editor.buffer().line(0).unwrap_or_default(), window, family, font_size);
        let selection = editor.selections().first().copied();
        let caret_col = selection.map_or(0, |s| s.head);
        let scroll = input_scroll(&layout, caret_col, width);
        let caret_x = layout.x_for_col(caret_col);

        let selection_rect = selection.filter(|s| !s.is_empty()).map(|s| {
            let from = layout.x_for_col(s.start());
            let to = layout.x_for_col(s.end());
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(from - scroll)
                .w(to - from)
                .bg(self.theme.selection)
        });
        let caret = active.then(|| {
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(caret_x - scroll)
                .w(px(2.))
                .bg(self.theme.caret)
                // Blinks with the editor's caret.
                .opacity(self.caret_opacity())
        });
        let bounds_cell = self.input_bounds[slot].clone();
        let invalid = slot == 0 && self.find.open && self.find.error.is_some();

        div()
            .id(id)
            .relative()
            .w(px(width))
            .h(px(line_height() + 4.0))
            .flex_none()
            .overflow_hidden()
            // A find box whose pattern does not parse shows the validation error colors.
            .bg(if invalid { self.theme.input.error_background } else { self.theme.input_background })
            .border_1()
            .border_color(if invalid {
                self.theme.input.error_border
            } else if active {
                self.theme.focus_border
            } else {
                self.theme.input.border.unwrap_or(self.theme.border)
            })
            .text_color(self.theme.input.foreground)
            .font_family(family)
            .text_size(px(font_size))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.click_input(focus, event.position, width, window, cx)
                }),
            )
            .child(
                canvas(|bounds, _, _| bounds, move |bounds, _, _, _| bounds_cell.set(bounds))
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1.))
                    .left(px(INPUT_PADDING))
                    .right(px(INPUT_PADDING))
                    .h(px(line_height()))
                    .children(selection_rect)
                    .children(editor.buffer().len_chars().eq(&0).then(|| {
                        focus.placeholder().map(|hint| {
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .whitespace_nowrap()
                                .text_color(self.theme.input.placeholder)
                                .child(hint)
                        })
                    }).flatten())
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left(-scroll)
                            .whitespace_nowrap()
                            .child(layout.styled(&[], &self.theme)),
                    )
                    .children(caret),
            )
    }
}

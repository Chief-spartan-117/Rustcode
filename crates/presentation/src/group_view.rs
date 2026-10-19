//! Drawing editor groups: the layout tree, each group's tab strip and line list, dividers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, uniform_list, AnyElement, Bounds, Context,
    ElementInputHandler, Entity, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Window,
};
use rustcode_domain::Selection;

use crate::editor_view::{EditorView, Focus, GUTTER_WIDTH};
use crate::groups::{drop_zone, Axis, DropZone, GroupId, Layout, PreviewMode, SplitDir};
use crate::text_layout::{line_overlays, LineLayout, line_height};

pub(crate) const TAB_BAR_HEIGHT: f32 = 34.0;
const DIVIDER: f32 = 4.0;
/// A group never gets less than this share of a split.
const MIN_SHARE: f32 = 0.08;

/// Payload of a tab being dragged.
pub(crate) struct DraggedTab {
    pub group: GroupId,
    pub index: usize,
    pub title: String,
}

/// What follows the pointer while dragging a tab.
struct TabPreview {
    title: String,
    background: gpui::Rgba,
    text: gpui::Rgba,
}

impl Render for TabPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.))
            .py(px(6.))
            .rounded(px(4.))
            .bg(self.background)
            .text_color(self.text)
            .opacity(0.9)
            .child(self.title.clone())
    }
}

/// A divider being dragged.
pub(crate) struct Resize {
    path: Vec<usize>,
    index: usize,
    axis: Axis,
    /// Pointer position along the axis at the start.
    start: Pixels,
    weights: Vec<f32>,
    /// Space the split's children share, in pixels.
    length: Pixels,
}

/// Bounds of every split container, by layout path, written during paint.
pub(crate) type SplitBounds = Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>;

impl EditorView {
    pub(crate) fn render_groups(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_layout(&self.layout, &mut Vec::new(), window, cx)
    }

    fn render_layout(
        &self,
        node: &Layout,
        path: &mut Vec<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Layout::Leaf(id) => {
                let index = self.group_index(*id).expect("group in layout exists");
                if self.is_empty_state() {
                    return self.render_empty_state(cx);
                }
                self.render_group(index, window, cx).into_any_element()
            }
            Layout::Split { axis, children, weights } => {
                let key = path.clone();
                let bounds_map = self.split_bounds.clone();
                let bounds_key = key.clone();
                let mut container = div().relative().flex_1().min_w_0().min_h_0().flex();
                container = match axis {
                    Axis::Horizontal => container.flex_row(),
                    Axis::Vertical => container.flex_col(),
                };
                let mut elements: Vec<AnyElement> = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    path.push(i);
                    let element = self.render_layout(child, path, window, cx);
                    path.pop();
                    let mut slot = div().min_w_0().min_h_0().flex().flex_row().child(element);
                    slot = slot.flex_basis(relative(0.));
                    slot.style().flex_grow = Some(weights[i]);
                    slot.style().flex_shrink = Some(1.);
                    elements.push(slot.into_any_element());
                    if i + 1 < children.len() {
                        elements.push(self.render_divider(&key, i, *axis, cx).into_any_element());
                    }
                }
                container
                    .child(
                        canvas(
                            |bounds, _, _| bounds,
                            move |bounds, _, _, _| {
                                bounds_map.borrow_mut().insert(bounds_key.clone(), bounds);
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .children(elements)
                    .into_any_element()
            }
        }
    }

    fn render_divider(
        &self,
        path: &[usize],
        index: usize,
        axis: Axis,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = path.to_vec();
        let id = path.iter().fold(index + 1, |acc, i| acc * 31 + i + 1);
        let hover = self.theme.selection;
        let base = div()
            .id(("group-divider", id))
            .flex_none()
            .bg(self.theme.group_border)
            .hover(move |style| style.bg(hover));
        let base = match axis {
            Axis::Horizontal => base.w(px(DIVIDER)).h_full().cursor_col_resize(),
            Axis::Vertical => base.h(px(DIVIDER)).w_full().cursor_row_resize(),
        };
        base.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                cx.stop_propagation();
                this.start_resize(path.clone(), index, axis, event, cx);
            }),
        )
    }

    fn start_resize(
        &mut self,
        path: Vec<usize>,
        index: usize,
        axis: Axis,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.split_bounds.borrow().get(&path).copied() else { return };
        let Some(Layout::Split { children, weights, .. }) = self.layout.node_mut(&path) else { return };
        let dividers = px(DIVIDER) * (children.len() - 1) as f32;
        let (start, length) = match axis {
            Axis::Horizontal => (event.position.x, bounds.size.width - dividers),
            Axis::Vertical => (event.position.y, bounds.size.height - dividers),
        };
        self.resize = Some(Resize { path, index, axis, start, weights: weights.clone(), length });
        cx.notify();
    }

    /// Drag a divider: the two groups next to it trade space.
    pub(crate) fn drag_resize(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(resize) = self.resize.as_ref() else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.resize = None;
            return;
        }
        let position = match resize.axis {
            Axis::Horizontal => event.position.x,
            Axis::Vertical => event.position.y,
        };
        let total: f32 = resize.weights.iter().sum();
        let moved = (position - resize.start) / resize.length.max(px(1.)) * total;
        let pair = resize.weights[resize.index] + resize.weights[resize.index + 1];
        let min = MIN_SHARE * total;
        let first = (resize.weights[resize.index] + moved).clamp(min, (pair - min).max(min));
        let (path, index) = (resize.path.clone(), resize.index);
        if let Some(Layout::Split { weights, .. }) = self.layout.node_mut(&path) {
            weights[index] = first;
            weights[index + 1] = pair - first;
        }
        cx.notify();
    }

    fn render_tab_bar(&self, gi: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let group = &self.groups[gi];
        let group_id = group.id;
        let group_active = gi == self.active_group;
        let hover = theme.lists.hover;
        let (preview_bg, preview_text) = (theme.widget_background, theme.text);

        let tabs: Vec<_> = (0..group.tabs.len()).map(|index| {
            let (title, dirty) = self.tab_label(gi, index);
            let tab_doc = group.tabs[index].doc;
            let selected = index == group.active;
            let unique = group_id.0 as usize * 1000 + index;
            let close = div()
                .id(("tab-close", unique))
                .w(px(18.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        if let Some(gi) = this.group_index(group_id) {
                            this.close_tab(gi, index, cx)
                        }
                    }),
                )
                .child(if dirty {
                    crate::icons::icon("dot", 12., if selected { theme.tabs.active_foreground } else { theme.tabs.inactive_foreground })
                } else {
                    crate::icons::icon("close", 12., theme.tabs.inactive_foreground)
                });
            let drag_title = title.clone();
            let mut tab = div()
                .id(("tab", unique))
                .h_full()
                .px(px(12.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .border_t_2()
                .border_color(match (selected, group_active) {
                    (true, true) => theme.tabs.active_border_top,
                    (true, false) => theme.tabs.unfocused_active_border_top,
                    _ => theme.tabs.bar_background,
                })
                .when_some(theme.tabs.border, |tab, color| tab.border_r_1().border_color(color))
                .text_color(if selected { theme.tabs.active_foreground } else { theme.tabs.inactive_foreground })
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        this.close_palette(true, cx);
                        if let Some(gi) = this.group_index(group_id) {
                            this.focus_view(gi, index, cx)
                        }
                    }),
                )
                .on_drag(
                    DraggedTab { group: group_id, index, title: drag_title },
                    move |dragged, _offset, _w, cx| {
                        let title = dragged.title.clone();
                        cx.new(|_| TabPreview { title, background: preview_bg, text: preview_text })
                    },
                )
                .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(hover))
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _w, cx| {
                    this.move_tab(dragged.group, dragged.index, group_id, Some(index), cx);
                }));
            tab = tab.bg(if selected { theme.tabs.active_background } else { theme.tabs.inactive_background });
            tab.child(self.webview_tab_icon(tab_doc).or_else(|| self.app_page_tab_icon(tab_doc, theme.tabs.inactive_foreground)).unwrap_or_else(|| self.entry_icon(&title, false, false, theme.tabs.inactive_foreground))).child(title).child(close)
        }).collect();
        let mode = group.preview;
        let is_page = crate::media_view::is_previewable(&self.doc_label(group.tabs[group.active].doc).0) && !self.is_webview_doc(group.tabs[group.active].doc) && !self.is_app_page(group.tabs[group.active].doc);
        // Side by side, in a new panel, or the page instead of the editor.
        let preview_buttons = is_page.then(|| {
            let button = |id: &'static str, icon: &'static str, on: bool, action: PreviewAction, cx: &mut Context<Self>| {
                div()
                    .id((id, group_id.0 as usize))
                    .flex_none()
                    .h_full()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .when(on, |b| b.bg(hover))
                    .hover(move |style| style.bg(hover))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                            if let Some(gi) = this.group_index(group_id) {
                                this.focus_view(gi, this.groups[gi].active, cx);
                                match action {
                                    PreviewAction::Side => this.toggle_markdown_preview(cx),
                                    PreviewAction::Panel => this.open_markdown_preview_panel(cx),
                                    PreviewAction::Page => this.toggle_markdown_full_preview(cx),
                                }
                            }
                        }),
                    )
                    .child(crate::icons::icon(icon, 16., theme.text))
            };
            div()
                .flex_none()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .px(px(4.))
                .child(button("tab-preview", "preview", mode == PreviewMode::Side, PreviewAction::Side, cx))
                .child(button("tab-preview-panel", "preview-panel", false, PreviewAction::Panel, cx))
                .child(button("tab-preview-page", "preview-page", mode == PreviewMode::Only, PreviewAction::Page, cx))
        });
        let tab_strip = div()
            .id(("tab-bar", group_id.0 as usize))
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&group.tab_scroll)
            .bg(theme.tabs.bar_background)
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(hover))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _w, cx| {
                this.move_tab(dragged.group, dragged.index, group_id, None, cx);
            }))
            .children(tabs)
            // The space after the last tab: a double click there opens a new file, as in VS Code.
            .child(
                div()
                    .id(("tab-bar-space", group_id.0 as usize))
                    .flex_1()
                    .h_full()
                    .min_w(px(24.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                            if event.click_count == 2 {
                                this.close_palette(true, cx);
                                if let Some(gi) = this.group_index(group_id) {
                                    this.focus_view(gi, this.groups[gi].active, cx);
                                }
                                this.new_file(cx);
                            }
                        }),
                    ),
            );
        div()
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .bg(theme.tabs.bar_background)
            .child(tab_strip)
            .children(preview_buttons)
    }

    /// Translucent preview of where a dragged tab would land.
    fn render_drop_overlay(&self, zone: DropZone) -> impl IntoElement {
        let overlay = div().absolute().bg(self.theme.selection).opacity(0.55);
        match zone {
            DropZone::Center => overlay.top_0().left_0().size_full(),
            DropZone::Split(SplitDir::Left) => overlay.top_0().left_0().w(relative(0.5)).h_full(),
            DropZone::Split(SplitDir::Right) => overlay.top_0().right_0().w(relative(0.5)).h_full(),
            DropZone::Split(SplitDir::Up) => overlay.top_0().left_0().w_full().h(relative(0.5)),
            DropZone::Split(SplitDir::Down) => overlay.bottom_0().left_0().w_full().h(relative(0.5)),
        }
    }

    /// Whether the document called `title` is shown with its lines wrapped.
    pub(crate) fn wraps(&self, title: &str) -> bool {
        // Soft wrap is the default: long lines continue on the next row instead of running past
        // the edge. `editor.wordWrap` or the toggle turns it off.
        self.word_wrap.or(self.editor_for(title).word_wrap).unwrap_or_else(|| {
            let title = title.to_ascii_lowercase();
            // Tests measure unwrapped columns in code files; the program itself wraps everything.
            !cfg!(test) || title.ends_with(".md") || title.ends_with(".markdown") || title.ends_with(".txt")
        })
    }

    /// Wrap long lines, or let them run on and scroll sideways.
    pub(crate) fn toggle_word_wrap(&mut self, cx: &mut Context<Self>) {
        let group = &self.groups[self.active_group];
        let now = self.wraps(&self.doc_label(group.tabs[group.active].doc).0);
        self.word_wrap = Some(!now);
        self.status = Some(if now { "Word wrap off" } else { "Word wrap on" }.to_owned());
        cx.notify();
    }

    /// The rows of group `gi`, when its text wraps or has folded lines.
    pub(crate) fn group_wrap(&self, gi: usize) -> Option<Rc<rustcode_domain::WrapMap>> {
        self.ensure_wrap(gi, px(self.char_width.get()))
    }

    /// Whether the rows of group `gi` hold long lines in several rows.
    fn group_wraps(&self, gi: usize) -> bool {
        self.groups[gi].wrap.borrow().as_ref().is_some_and(|m| m.wraps())
    }

    /// Bring the rows of group `gi` up to date with its document and width.
    fn ensure_wrap(&self, gi: usize, char_width: Pixels) -> Option<Rc<rustcode_domain::WrapMap>> {
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let (document, _) = self.doc_ref(doc);
        let width = f32::from(group.bounds.get().size.width) - GUTTER_WIDTH - 16.0;
        let char_width = f32::from(char_width);
        let folded = self.collapsed_of(doc);
        let wrapping = self.wraps(&document.title) && width >= 80.0 && char_width > 0.0;
        if !wrapping && folded.is_empty() {
            *group.wrap.borrow_mut() = None;
            return None;
        }
        // Without wrapping, only folded lines leave the rows.
        let cols = if wrapping { (width / char_width).floor() as usize } else { rustcode_domain::NO_WRAP };
        let revision = document.editor.revision();
        let indent = self.editor_for(&document.title).wrapping_indent.unwrap_or(true);
        let mut slot = group.wrap.borrow_mut();
        if let Some(map) = slot.as_ref() {
            if group.wrap_doc.get() == Some(doc) && map.cols() == cols && map.revision() == revision && map.folded() == folded && map.indents_wrapped_rows() == indent {
                return Some(map.clone());
            }
        }
        let map = Rc::new(rustcode_domain::WrapMap::build_with(document.editor.buffer(), cols, revision, folded, indent));
        *slot = Some(map.clone());
        group.wrap_doc.set(Some(doc));
        Some(map)
    }

    /// Longest line of the document in `group`, in chars. Kept until the next edit.
    fn longest_line_chars(&self, doc: rustcode_domain::DocumentId) -> usize {
        let (document, _) = self.doc_ref(doc);
        let revision = document.editor.revision();
        if let Some((id, rev, chars)) = self.longest_line.get() {
            if id == doc && rev == revision {
                return chars;
            }
        }
        let buffer = document.editor.buffer();
        let chars = (0..buffer.line_count()).map(|i| buffer.line_len_chars(i)).max().unwrap_or(0);
        self.longest_line.set(Some((doc, revision, chars)));
        chars
    }

    /// Furthest the text of group `gi` can be scrolled sideways.
    fn max_scroll_x(&self, gi: usize, char_width: Pixels) -> f32 {
        let group = &self.groups[gi];
        if self.group_wraps(gi) {
            return 0.0;
        }
        let chars = self.longest_line_chars(group.tabs[group.active].doc);
        let viewport = f32::from(group.bounds.get().size.width) - GUTTER_WIDTH;
        (f32::from(char_width) * chars as f32 + 32.0 - viewport).max(0.0)
    }

    fn render_group(&self, gi: usize, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (text_color, gutter_color, selection_color, caret_color) =
            (theme.text, theme.gutter_text, theme.selection, theme.caret);
        let gutter_bg = theme.gutter.background;
        let active_line_number = theme.gutter.active_line_number;
        let (gutter_added, gutter_modified, gutter_deleted) = (theme.gutter.added, theme.gutter.modified, theme.gutter.deleted);
        let diff = (
            theme.diff.inserted_line,
            theme.diff.removed_line,
            theme.diff.header_line,
            theme.diff.current_header,
            theme.diff.current_content,
            theme.diff.incoming_content,
            theme.diff.incoming_header,
        );
        let (find_current, find_other) = (theme.find_match, theme.find_match_highlight);
        let line_bg = theme.line_highlight;
        let debug_colors = (theme.debug.breakpoint, theme.debug.breakpoint_unverified, theme.debug.stack_arrow);
        let caret_opacity = self.caret_opacity();
        let line_break_width = LineLayout::new("m", window).width();
        let group = &self.groups[gi];
        let group_id = group.id;
        let is_active = gi == self.active_group;
        let takes_input = is_active && self.focus != Focus::Terminal;
        let (document, _) = self.doc_ref(group.tabs[group.active].doc);
        let line_count = document.editor.buffer().line_count();

        self.char_width.set(f32::from(line_break_width));
        let wrap = self.ensure_wrap(gi, line_break_width);
        let row_count = wrap.as_ref().map_or(line_count, |w| w.rows());
        let wrapping = wrap.as_ref().is_some_and(|w| w.wraps());

        // Sideways scroll: keep it in range, and bring the caret into view when it moves. Wrapped
        // text never scrolls sideways.
        let max_x = if wrapping { 0.0 } else { self.max_scroll_x(gi, line_break_width) };
        group.scroll_x.set(group.scroll_x.get().clamp(0.0, max_x));
        if is_active && !wrapping {
            if let Some(head) = document.editor.selections().last().map(|s| s.head) {
                if head != group.scroll_x_caret.get() {
                    group.scroll_x_caret.set(head);
                    let buffer = document.editor.buffer();
                    let line = buffer.char_to_line(head);
                    let layout = self.layout_line(group.tabs[group.active].doc, line, &buffer.line(line).unwrap_or_default(), window);
                    let caret_x = f32::from(layout.x_for_col(head - buffer.line_to_char(line)));
                    let viewport = f32::from(group.bounds.get().size.width) - GUTTER_WIDTH;
                    let x = group.scroll_x.get();
                    if viewport > 80.0 {
                        if caret_x < x {
                            group.scroll_x.set((caret_x - 40.0).max(0.0));
                        } else if caret_x > x + viewport - 24.0 {
                            group.scroll_x.set((caret_x - viewport + 64.0).min(max_x));
                        }
                    }
                }
            }
        }
        let scroll_x = group.scroll_x.get();

        let wrap_rows = wrap.clone();
        // Lines that differ from the last commit get a mark in the gutter.
        let line_diff = self.line_changes_of(group.tabs[group.active].doc);
        let doc_id = group.tabs[group.active].doc;
        // Side-by-side diff: tints, and both sides scroll together.
        let pair_tint = self.pair_tint(doc_id);
        let pair_is_old = self.git_state.diff_pair.is_some_and(|(old, _)| old == doc_id);
        if let (Some((old, new)), true) = (self.git_state.diff_pair, is_active && pair_tint.is_some()) {
            let partner = if pair_is_old { new } else { old };
            if let Some(other) = self.groups.iter().position(|g| g.tabs[g.active].doc == partner) {
                let offset = group.scroll.0.borrow().base_handle.offset();
                self.groups[other].scroll.0.borrow().base_handle.set_offset(offset);
            }
        }
        let conflicts = self.conflicts_of(doc_id);
        let blame = if self.blame_mode == crate::git_extra::BlameMode::Off { None } else { self.blame_lines_of(doc_id) };
        let blame_mode = self.blame_mode;
        let annotation_color = theme.gutter_text;
        // A diff document tints its added and removed lines.
        let is_diff = document.title.ends_with(".diff");
        let lines = uniform_list(
            ("editor-lines", group_id.0 as usize),
            row_count,
            cx.processor(move |this: &mut Self, range: Range<usize>, window, _cx| {
                let group = &this.groups[gi];
                let tab = &group.tabs[group.active];
                let (document, highlighter) = this.doc_ref(tab.doc);
                let editor = &document.editor;
                let folds = this.features.folds.get(&tab.doc).map(|s| &s.folds);
                let language = this.syntax.detect_language(&document.title).map(|l| l.to_string());
                let doc_links = this.links_of(tab.doc);
                let debug_path = document.path.as_deref();
                let kinds = debug_path.map(|p| this.debug.kinds_of(p)).unwrap_or_default();
                let verified = debug_path.and_then(|p| this.debug.verified.get(p));
                let stopped_at = this.debug.current_location().filter(|(p, _)| Some(*p) == debug_path).map(|(_, line)| line);
                let debug_live = this.debug.is_live();
                let marks = document
                    .path
                    .as_deref()
                    .map(|p| this.problems.lines_of(p))
                    .unwrap_or_default();
                let file_problems: Vec<&rustcode_application::Diagnostic> = document
                    .path
                    .as_deref()
                    .map(|p| this.problems.items.iter().filter(|d| d.path == p).collect())
                    .unwrap_or_default();
                // The active view's selections live in the editor; other views keep their own.
                let selections: &[Selection] =
                    if gi == this.active_group { editor.selections() } else { &tab.view.selections };
                // Matches belong to the installed document.
                let (matches, current) = if this.find.open && tab.doc == this.document.id {
                    (this.find.matches.as_slice(), this.find.current_range())
                } else {
                    (&[][..], None)
                };
                // Brackets, the pair by the caret and the guides for the lines on screen.
                let (first_line, last_line) = match wrap_rows.as_ref() {
                    Some(w) if !range.is_empty() => {
                        (w.segment(editor.buffer(), range.start).line, w.segment(editor.buffer(), range.end - 1).line)
                    }
                    _ => (range.start, range.end.saturating_sub(1)),
                };
                let bracket_frame = this.bracket_frame(tab.doc, highlighter, editor.buffer(), selections, &document.title, first_line, last_line);
                // What extensions decorated is found by the name they know the file by.
                let deco_path = if this.ext_decorations.is_empty() { None } else { crate::extensions::ext_path_of(document) };
                range
                    .map(|row| {
                        // With wrapping, a row is a stretch of a line; `index` is the line.
                        let seg = wrap_rows.as_ref().map(|w| w.segment(editor.buffer(), row));
                        let index = seg.map_or(row, |s| s.line);
                        let first = seg.map_or(true, |s| s.first);
                        let bp_kind = if first { kinds.get(&index).copied() } else { None };
                        // A breakpoint the debugger could not place is grey.
                        let placed = !debug_live || verified.is_none_or(|v| v.get(&index).copied().unwrap_or(true));
                        let on_stack_line = first && stopped_at == Some(index);
                        let changed = line_diff.as_ref().and_then(|c| rustcode_domain::change_at(c, index));
                        // Who changed the line, shown after its text.
                        let mut annotation: Option<String> = blame.as_ref().and_then(|(lines, map)| {
                            let current = gi == this.active_group
                                && crate::text_layout::line_has_caret(selections, editor.buffer(), index);
                            if (blame_mode == crate::git_extra::BlameMode::Current && !current)
                                || seg.is_some_and(|s| !s.last)
                            {
                                return None;
                            }
                            match map.get(index)? {
                                Some(old) => lines.get(*old).map(crate::git_extra::blame_text),
                                None => Some("You · uncommitted changes".to_owned()),
                            }
                        });
                        let line_chars = editor.buffer().line_len_chars(index);
                        let mut decor = this.line_decor(deco_path.as_deref(), index, line_chars);
                        // Other uses of the symbol at the caret.
                        let mark = this.theme.find_match_highlight;
                        decor.runs.extend(this.highlight_runs(tab.doc, index, editor.buffer()).into_iter().map(|r| (r, None, Some(mark))));
                        // Text extensions put after a line shows beside who changed it.
                        if let Some(extra) = decor.after.clone().filter(|_| seg.is_none_or(|s| s.last)) {
                            annotation = Some(match annotation.take() {
                                Some(blame) => format!("{blame}  {extra}"),
                                None => extra,
                            });
                        }
                        let row_tint = {
                            use rustcode_domain::LineChangeKind;
                            let paired = pair_tint.as_ref().and_then(|t| rustcode_domain::change_at(t, index)).and_then(|k| {
                                (k != LineChangeKind::Deleted).then(|| {
                                    if pair_is_old { diff.1 } else { diff.0 }
                                })
                            });
                            let in_conflict = conflicts.iter().find(|c| c.contains(index)).map(|c| {
                                if index == c.start {
                                    diff.3
                                } else if index == c.middle || index == c.end {
                                    diff.6
                                } else if index < c.middle {
                                    diff.4
                                } else {
                                    diff.5
                                }
                            });
                            paired.or(in_conflict).or(on_stack_line.then_some(this.theme.debug.stack_line)).or(decor.whole_line)
                        };
                        let raw = editor.buffer().line(index).unwrap_or_default();
                        let layout = this.layout_line(tab.doc, index, &raw, window);
                        let fold_start = folds.and_then(|f| f.range_at(index)).filter(|_| first);
                        let is_folded = folds.is_some_and(|f| f.is_folded(index)) && seg.is_none_or(|s| s.last);
                        let (x_start, seg_width, clip_left) = match seg.filter(|_| wrapping) {
                            Some(s) => {
                                // A row after the first starts `s.indent` cells in: its text is
                                // drawn that far from the left edge, so its clip begins there.
                                let from = layout.x_for_col(s.start);
                                let to = if s.last {
                                    layout.width() + line_break_width + px(4.)
                                        + annotation.as_ref().map_or(px(0.), |a| line_break_width * (a.chars().count() as f32 + 6.0))
                                } else {
                                    layout.x_for_col(s.end)
                                };
                                (from, Some(to - from), line_break_width * s.indent as f32)
                            }
                            None => (px(0.), None, px(0.)),
                        };
                        // The line with the cursor stands out. A selection already marks its own lines.
                        let has_caret = gi == this.active_group
                            && crate::text_layout::line_has_caret(selections, editor.buffer(), index);
                        let mut overlays = line_overlays(
                            &layout,
                            editor.buffer(),
                            selections,
                            matches,
                            current.as_ref(),
                            index,
                            line_break_width,
                        );
                        if gi != this.active_group {
                            overlays.carets.clear();
                        }
                        let grammar_spans = highlighter.map_or(&[][..], |h| h.line(index));
                        let semantic = this.semantic_spans(tab.doc, index, language.as_deref());
                        let merged;
                        let spans = if semantic.is_empty() {
                            grammar_spans
                        } else {
                            merged = rustcode_domain::overlay_spans(grammar_spans, &semantic);
                            &merged[..]
                        };
                        let squiggles: Vec<_> = crate::problems::squiggles_on_line(&file_problems, index, layout.char_count())
                            .into_iter()
                            .map(|(range, severity)| (range, crate::problems::severity_color(&this.theme, severity)))
                            .collect();
                        let link_ranges: Vec<Range<usize>> = doc_links
                            .iter()
                            .filter(|l| l.target.is_some() && l.line <= index && index <= l.end_line)
                            .map(|l| {
                                let start = if index == l.line { l.column } else { 0 };
                                let end = if index == l.end_line { l.end_column } else { layout.char_count() };
                                start..end
                            })
                            .collect();
                        let bracket_colors = bracket_frame.colors(index, &this.theme);
                        let text = layout.styled_with_marks(spans, &bracket_colors, &squiggles, &link_ranges, &decor.runs, &this.theme);
                        // Guides sit one cell apart, so they use the width of one character.
                        let guides: Vec<(Pixels, gpui::Rgba)> = bracket_frame
                            .guides(editor.buffer(), index, &raw, &this.theme)
                            .into_iter()
                            .map(|(cell, color)| (line_break_width * cell as f32, color))
                            .collect();
                        // Horizontal segments only on the first row of a line.
                        let horizontals: Vec<(Pixels, Pixels, gpui::Rgba)> = bracket_frame
                            .horizontals(index, &this.theme)
                            .into_iter()
                            .filter(|_| first)
                            .map(|(cell, col, color)| (line_break_width * cell as f32, layout.x_for_col(col), color))
                            .collect();
                        let bracket_boxes: Vec<(Pixels, Pixels)> = bracket_frame
                            .matched_columns(index)
                            .into_iter()
                            .map(|(col, len)| {
                                let left = layout.x_for_col(col);
                                (left, layout.x_for_col(col + len) - left)
                            })
                            .collect();
                        let (match_bg, match_border) = (this.theme.brackets.match_background, this.theme.brackets.match_border);
                        // Frames extensions drew around ranges (`border`, `outline`).
                        let frames: Vec<(Pixels, Pixels, crate::ext_decorations::Edge, bool)> = decor
                            .frames
                            .iter()
                            .map(|(range, edge, rounded)| {
                                let left = layout.x_for_col(range.start);
                                (left, layout.x_for_col(range.end) - left, *edge, *rounded)
                            })
                            .collect();
                        // Lenses go at the end of the line, after whatever else is there.
                        let lens_items = if seg.is_none_or(|s| s.last) { this.lenses_on_line(tab.doc, index) } else { Vec::new() };
                        let annotation_cells = annotation.as_ref().map_or(0, |a| a.chars().count() + 3);
                        let lens_doc = tab.doc;
                        div()
                            .h(px(line_height()))
                            // Full width, so the current-line band reaches the right edge.
                            .w_full()
                            .flex()
                            .flex_row()
                            .font_family(crate::text_layout::code_font())
                            .text_size(px(crate::text_layout::font_size()))
                            .child(
                                div()
                                    .relative()
                                    .group(format!("gutter-{gi}-{index}"))
                                    .w(px(GUTTER_WIDTH))
                                    .flex_none()
                                    .pr(px(12.))
                                    .bg(gutter_bg)
                                    .text_color(if has_caret { active_line_number } else { gutter_color })
                                    .text_right()
                                    .children(changed.filter(|k| first || *k != rustcode_domain::LineChangeKind::Deleted).map(|kind| {
                                        use rustcode_domain::LineChangeKind::*;
                                        let (color, deleted) = match kind {
                                            Added => (gutter_added, false),
                                            Modified => (gutter_modified, false),
                                            Deleted => (gutter_deleted, true),
                                        };
                                        // Added and modified lines get a bar; a removal, a small wedge. Click
                                        // the mark to stage that change; alt-click puts it back as committed.
                                        let mark = if deleted {
                                            div().absolute().top_0().right(px(3.)).w(px(8.)).h(px(2.)).bg(color)
                                        } else {
                                            div().absolute().top_0().right(px(3.)).w(px(3.)).h_full().bg(color)
                                        };
                                        div()
                                            .id(("change-mark", index))
                                            .absolute()
                                            .top_0()
                                            .right_0()
                                            .w(px(10.))
                                            .h_full()
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                _cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                                                    cx.stop_propagation();
                                                    // The change belongs to this group's file.
                                                    this.focus_view(gi, this.groups[gi].active, cx);
                                                    if event.modifiers.alt {
                                                        this.git_revert_change(Some(index), cx);
                                                    } else {
                                                        this.git_stage_hunk_at(Some(index), cx);
                                                    }
                                                }),
                                            )
                                            .child(mark)
                                    }))
                                    .children(marks.get(&index).filter(|_| first).map(|severity| {
                                        div()
                                            .absolute()
                                            .left(px(16.))
                                            .top(px(7.))
                                            .w(px(6.))
                                            .h(px(6.))
                                            .rounded(px(3.))
                                            .bg(crate::problems::severity_color(&this.theme, *severity))
                                    }))
                                    .child({
                                        // Click beside the line number for a breakpoint; a faint dot shows where.
                                        let name = format!("gutter-{gi}-{index}");
                                        let dot = if placed { debug_colors.0 } else { debug_colors.1 };
                                        div()
                                            .id(("breakpoint-mark", index))
                                            .absolute()
                                            .left_0()
                                            .top_0()
                                            .w(px(15.))
                                            .h(px(line_height()))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                _cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                                    cx.stop_propagation();
                                                    this.focus_view(gi, this.groups[gi].active, cx);
                                                    if let Some(path) = this.document.path.clone() {
                                                        this.toggle_breakpoint_at(&path, index, cx);
                                                    }
                                                }),
                                            )
                                            .child({
                                                let mark = div().flex().items_center().justify_center();
                                                match bp_kind {
                                                    // A message, not a stop: a diamond.
                                                    Some(crate::debug_view::BreakpointKind::Log) => mark
                                                        .text_size(px(11.))
                                                        .text_color(dot)
                                                        .child("\u{25C6}")
                                                        .into_any_element(),
                                                    Some(kind) => mark
                                                        .w(px(9.))
                                                        .h(px(9.))
                                                        .rounded(px(5.))
                                                        .bg(dot)
                                                        // A condition or a hit count: a pale dot inside.
                                                        .children((kind == crate::debug_view::BreakpointKind::Conditional).then(|| {
                                                            div().w(px(3.)).h(px(3.)).rounded(px(2.)).bg(gpui::rgb(0xffffff))
                                                        }))
                                                        .into_any_element(),
                                                    None => mark
                                                        .w(px(9.))
                                                        .h(px(9.))
                                                        .rounded(px(5.))
                                                        .bg(dot)
                                                        .opacity(0.)
                                                        .group_hover(name, |style| style.opacity(0.3))
                                                        .into_any_element(),
                                                }
                                            })
                                            .when(on_stack_line, |d| d.child(div().absolute().child(crate::icons::icon("chevron-right", 12., debug_colors.2))))
                                    })
                                    .children(fold_start.map(|_| {
                                        let name = format!("gutter-{gi}-{index}");
                                        div()
                                            .id(("fold-mark", index))
                                            .absolute()
                                            .left(px(24.))
                                            .top_0()
                                            .w(px(14.))
                                            .h(px(line_height()))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .when(!is_folded, |mark| mark.opacity(0.).group_hover(name, |style| style.opacity(1.)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                _cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                                    cx.stop_propagation();
                                                    this.focus_view(gi, this.groups[gi].active, cx);
                                                    this.toggle_fold_line(index, cx);
                                                }),
                                            )
                                            .child(crate::icons::icon(if is_folded { "chevron-right" } else { "chevron-down" }, 12., gutter_color))
                                    }))
                                    .child(if first { (index + 1).to_string() } else { String::new() }),
                            )
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .h_full()
                                    .overflow_hidden()
                                    .when(has_caret, |code| code.bg(line_bg))
                                    .when_some(row_tint, |code, color| code.bg(color))
                                    .when(is_diff, |code| {
                                        let tint = match raw.chars().next() {
                                            Some('+') if !raw.starts_with("+++") => Some(diff.0),
                                            Some('-') if !raw.starts_with("---") => Some(diff.1),
                                            Some('@') => Some(diff.2),
                                            _ => None,
                                        };
                                        match tint {
                                            Some(color) => code.bg(color),
                                            None => code,
                                        }
                                    })
                                    // The guides span the row, indent included, so they do not move with the text.
                                    .child(div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .h_full()
                                        .overflow_hidden()
                                        .when_some(seg_width, |clip, width| clip.w(width + clip_left))
                                        .when(seg_width.is_none(), |clip| clip.w_full())
                                        .child(div().absolute().top_0().left(px(-scroll_x)).size_full()
                                            .children(guides.into_iter().map(|(left, color)| {
                                                div().absolute().top_0().left(left).w(px(1.)).h_full().bg(color)
                                            }))
                                            .children(horizontals.into_iter().map(|(from, to, color)| {
                                                div().absolute().top_0().left(from).w((to - from).max(px(1.))).h(px(1.)).bg(color)
                                            }))))
                                    .child(div()
                                        .absolute()
                                        .top_0()
                                        .left(clip_left)
                                        .h_full()
                                        .cursor_text()
                                        .overflow_hidden()
                                        .when_some(seg_width, |clip, width| clip.w(width))
                                        .when(seg_width.is_none(), |clip| clip.w_full())
                                        .child(div().absolute().top_0().left(px(-scroll_x) - x_start).size_full()
                                    .children(bracket_boxes.into_iter().map(|(left, width)| {
                                        div()
                                            .absolute()
                                            .top(px(1.))
                                            .left(left)
                                            .w(width)
                                            .h(px(line_height() - 2.))
                                            .bg(match_bg)
                                            .border_1()
                                            .border_color(match_border)
                                    }))
                                    .children(frames.into_iter().map(|(left, width, edge, rounded)| {
                                        div()
                                            .absolute()
                                            .top_0()
                                            .left(left)
                                            .w(width)
                                            .h(px(line_height()))
                                            .border_color(edge.color)
                                            .when(edge.width <= 1, |d| d.border_1())
                                            .when(edge.width == 2, |d| d.border_2())
                                            .when(edge.width > 2, |d| d.border_4())
                                            .when(edge.dashed, |d| d.border_dashed())
                                            .when(rounded, |d| d.rounded(px(3.)))
                                    }))
                                    .children(annotation.map(|text| {
                                        div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .left(layout.width() + line_break_width * 4.0)
                                            .whitespace_nowrap()
                                            .text_size(px(crate::text_layout::font_size() - 1.0))
                                            .text_color(annotation_color)
                                            .opacity(0.6)
                                            .italic()
                                            .child(text)
                                    }))
                                    .children((!lens_items.is_empty()).then(|| {
                                        div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .left(layout.width() + line_break_width * (4.0 + annotation_cells as f32))
                                            .flex()
                                            .flex_row()
                                            .gap(px(10.))
                                            .whitespace_nowrap()
                                            .text_size(px(crate::text_layout::font_size() - 1.0))
                                            .text_color(annotation_color)
                                            .children(lens_items.into_iter().map(|(number, title)| {
                                                div()
                                                    .id(("lens", index * 100 + number))
                                                    .cursor_pointer()
                                                    .hover(|style| style.underline().text_color(gpui::rgb(0x3794ff)))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        _cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                                            cx.stop_propagation();
                                                            this.run_lens(lens_doc, number, cx)
                                                        }),
                                                    )
                                                    .child(title)
                                            }))
                                    }))
                                    .children(is_folded.then(|| {
                                        div()
                                            .id(("fold-badge", index))
                                            .absolute()
                                            .top(px(2.))
                                            .left(layout.width() + px(8.))
                                            .h(px(line_height() - 4.))
                                            .px(px(6.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(3.))
                                            .bg(selection_color)
                                            .text_color(gutter_color)
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                _cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                                    cx.stop_propagation();
                                                    this.focus_view(gi, this.groups[gi].active, cx);
                                                    this.toggle_fold_line(index, cx);
                                                }),
                                            )
                                            .child("\u{22ef}")
                                    }))
                                    .children(overlays.matches.into_iter().map(
                                        |(left, width, is_current)| {
                                            div()
                                                .absolute()
                                                .top_0()
                                                .h_full()
                                                .left(left)
                                                .w(width)
                                                .bg(if is_current { find_current } else { find_other })
                                        },
                                    ))
                                    .children(overlays.selections.into_iter().map(
                                        |(left, width)| {
                                            div()
                                                .absolute()
                                                .top_0()
                                                .h_full()
                                                .left(left)
                                                .w(width)
                                                .bg(selection_color)
                                        },
                                    ))
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .left_0()
                                            .whitespace_nowrap()
                                            .text_color(text_color)
                                            .child(text),
                                    )
                                    .children(overlays.carets.into_iter().map(|left| {
                                        div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .left(left)
                                            .w(px(2.))
                                            .bg(caret_color)
                                            .opacity(caret_opacity)
                                    })))),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(group.scroll.clone())
        .size_full();

        let entity: Entity<Self> = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let bounds_cell = group.bounds.clone();
        // Records the list viewport for mouse mapping; the active group takes text input.
        let input_layer = canvas(
            |bounds, _window, _cx| bounds,
            move |bounds, _, window, cx| {
                bounds_cell.set(bounds);
                if takes_input {
                    window.handle_input(&focus_handle, ElementInputHandler::new(bounds, entity), cx);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        // Sits in front of the list, which keeps the wheel events it handles from reaching the
        // view; this one only reads the sideways part.
        let wheel_layer = div().absolute().top_0().left_0().size_full()
            .on_scroll_wheel(cx.listener(move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                // The list scrolls up and down by itself; sideways is handled here.
                let delta = event.delta.pixel_delta(px(line_height()));
                let dx = f32::from(delta.x);
                if dx == 0.0 {
                    return;
                }
                // A gesture that is mostly sideways must not also scroll the list up or down.
                if delta.x.abs() > delta.y.abs() {
                    cx.stop_propagation();
                }
                if let Some(gi) = this.group_index(group_id) {
                    let max = this.max_scroll_x(gi, LineLayout::new("m", window).width());
                    let group = &this.groups[gi];
                    let next = (group.scroll_x.get() - dx).clamp(0.0, max);
                    if next != group.scroll_x.get() {
                        group.scroll_x.set(next);
                        cx.notify();
                    }
                }
            }))
;

        let body = div()
            .id(("group-body", group_id.0 as usize))
            .relative()
            .flex_1()
            .min_h_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if let Some(gi) = this.group_index(group_id) {
                        this.focus_view(gi, this.groups[gi].active, cx);
                    }
                    this.on_mouse_down(event, window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if let Some(gi) = this.group_index(group_id) {
                        this.focus_view(gi, this.groups[gi].active, cx);
                    }
                    this.open_editor_context_menu(event, window, cx)
                }),
            )
            .on_drag_move::<DraggedTab>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<DraggedTab>, _w, cx| {
                    // Every group hears about the drag; only the one under the pointer reacts.
                    let hint = event
                        .bounds
                        .contains(&event.event.position)
                        .then(|| (group_id, drop_zone(event.bounds, event.event.position)));
                    let current = this.drop_hint;
                    if hint.is_some() && current != hint {
                        this.drop_hint = hint;
                        cx.notify();
                    } else if hint.is_none() && current.is_some_and(|(id, _)| id == group_id) {
                        this.drop_hint = None;
                        cx.notify();
                    }
                },
            ))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _w, cx| {
                let zone = this
                    .drop_hint
                    .filter(|(id, _)| *id == group_id)
                    .map_or(DropZone::Center, |(_, zone)| zone);
                this.drop_tab(dragged.group, dragged.index, group_id, zone, cx);
            }))
            .on_drop(cx.listener(move |this, dragged: &crate::sidebar::DraggedEntries, _w, cx| {
                if let Some(gi) = this.group_index(group_id) {
                    this.focus_view(gi, this.groups[gi].active, cx);
                }
                for path in &dragged.files {
                    this.open_path(path, cx);
                }
            }))
            .child(lines)
            .child(wheel_layer)
            .child(input_layer)
            .children({
                let editor_scroll = group.scroll.0.borrow().base_handle.clone();
                self.scrollbar(100 + gi, &editor_scroll, cx)
            })
            .children(
                self.drop_hint
                    .filter(|(id, _)| *id == group_id)
                    .map(|(_, zone)| self.render_drop_overlay(zone)),
            );

        div()
            .id(("group", group_id.0 as usize))
            .on_drop(cx.listener(move |this, dropped: &gpui::ExternalPaths, _w, cx| {
                if let Some(gi) = this.group_index(group_id) {
                    this.focus_view(gi, this.groups[gi].active, cx);
                }
                this.open_dropped_paths(dropped.paths(), cx);
            }))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_tab_bar(gi, cx))
            .children(self.render_breadcrumbs(gi, cx))
            .children(self.render_flutter_bar(gi, cx))
            .children((is_active && self.find.open).then(|| self.render_find_bar(window, cx)))
            .child({
                let title = self.doc_label(group.tabs[group.active].doc).0;
                let page = crate::media_view::is_previewable(&title) && !self.is_webview_doc(group.tabs[group.active].doc) && !self.is_app_page(group.tabs[group.active].doc);
                let mut row = div().flex_1().min_h_0().flex().flex_row();
                let preview = |this: &Self, cx: &mut Context<Self>| -> AnyElement {
                    if crate::media_view::is_svg(&title) {
                        this.render_svg_preview(gi, window.scale_factor())
                    } else {
                        this.render_markdown_preview(gi, cx).into_any_element()
                    }
                };
                if self.is_app_page(group.tabs[group.active].doc) {
                    row = row.child(self.render_app_page(gi, window, cx));
                } else if self.is_webview_doc(group.tabs[group.active].doc) {
                    row = row.child(self.render_webview(gi, cx));
                } else if self.is_media_doc(group.tabs[group.active].doc) {
                    row = row.child(self.render_media(gi, cx));
                } else if self.notebooks.contains_key(&group.tabs[group.active].doc) {
                    row = row.child(self.render_notebook(gi, window, cx));
                } else {
                    match group.preview {
                        PreviewMode::Only if page => row = row.child(preview(self, cx)),
                        PreviewMode::Side if page => row = row.child(body).children(self.render_minimap(gi, cx)).child(preview(self, cx)),
                        _ => row = row.child(body).children(self.render_minimap(gi, cx)),
                    }
                }
                row
            })
    }
}

#[derive(Clone, Copy)]
enum PreviewAction {
    Side,
    Panel,
    Page,
}

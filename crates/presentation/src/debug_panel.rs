//! The Debug tab of the panel (breakpoints, call stack, variables with watches, console) and the
//! floating bar with the buttons to carry on and step.

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent, MouseUpEvent, ScrollHandle};
use rustcode_application::DebugCommand;

use crate::debug_view::{DebugPhase, DebugState};
use crate::editor_view::EditorView;
use crate::shell::ROW_HEIGHT;

/// The little label that shows when the pointer rests on a button of the bar.
struct Tip(&'static str);

impl gpui::Render for Tip {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(gpui::rgb(0x252526))
            .border_1()
            .border_color(gpui::rgb(0x454545))
            .text_color(gpui::rgb(0xcccccc))
            .text_size(px(12.))
            .child(self.0)
    }
}

/// A row of the variable tree: how deep, name, value, type, the reference to open it by, and
/// whether it is open.
struct VariableRow {
    depth: usize,
    name: String,
    value: Option<String>,
    kind: Option<String>,
    reference: i64,
    open: bool,
}

impl EditorView {
    fn variable_rows(&self) -> Vec<VariableRow> {
        fn add(state: &DebugState, reference: i64, depth: usize, out: &mut Vec<VariableRow>) {
            for v in state.variables.get(&reference).into_iter().flatten() {
                let open = v.reference > 0 && state.expanded.contains(&v.reference);
                out.push(VariableRow { depth, name: v.name.clone(), value: Some(v.value.clone()), kind: v.kind.clone(), reference: v.reference, open });
                if open && depth < 8 {
                    add(state, v.reference, depth + 1, out);
                }
            }
        }
        let mut rows = Vec::new();
        for scope in &self.debug.scopes {
            let open = self.debug.expanded.contains(&scope.reference);
            rows.push(VariableRow { depth: 0, name: scope.name.clone(), value: None, kind: None, reference: scope.reference, open });
            if open {
                add(&self.debug, scope.reference, 1, &mut rows);
            }
        }
        rows
    }

    pub(crate) fn render_debug(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (text, dim, hover, border) = (theme.text, theme.gutter_text, theme.lists.hover, theme.border);
        let phase = self.debug.phase;
        let status = match phase {
            DebugPhase::Idle => "Press F5 to start debugging".to_owned(),
            DebugPhase::Starting => "Starting...".to_owned(),
            DebugPhase::Running => format!("Running {}", self.debug.config_name.clone().unwrap_or_default()),
            DebugPhase::Stopped => format!("Paused on {}", self.debug.stop_reason.clone().unwrap_or_default()),
        };
        let button = |id: &'static str, label: &'static str, enabled: bool, run: fn(&mut EditorView, &mut Context<EditorView>)| {
            div()
                .id(id)
                .px(px(8.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .text_color(if enabled { text } else { dim })
                .when(enabled, |b| b.cursor_pointer().hover(move |style| style.bg(hover)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        if enabled {
                            run(this, cx);
                        }
                    }),
                )
                .child(label)
        };
        let stopped = phase == DebugPhase::Stopped;
        let live = phase != DebugPhase::Idle;
        let toolbar = div()
            .h(px(28.))
            .flex_none()
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(border)
            .child(if live {
                button("debug-continue", if stopped { "Continue" } else { "Pause" }, phase != DebugPhase::Starting, |this, cx| {
                    if this.debug.phase == DebugPhase::Stopped {
                        this.debug_command(DebugCommand::Continue, cx)
                    } else {
                        this.debug_command(DebugCommand::Pause, cx)
                    }
                })
            } else {
                button("debug-start", "Start", true, |this, cx| this.start_debugging(cx))
            })
            .child(button("debug-over", "Step Over", stopped, |this, cx| this.debug_command(DebugCommand::Next, cx)))
            .child(button("debug-into", "Step Into", stopped, |this, cx| this.debug_command(DebugCommand::StepIn, cx)))
            .child(button("debug-out", "Step Out", stopped, |this, cx| this.debug_command(DebugCommand::StepOut, cx)))
            .child(button("debug-restart", "Restart", live, |this, cx| this.restart_debugging(cx)))
            .child(button("debug-stop", "Stop", live, |this, cx| this.stop_debugging(cx)))
            .child(div().ml(px(10.)).text_color(dim).child(status));

        let header = |label: &'static str| {
            div().h(px(ROW_HEIGHT)).flex_none().px(px(10.)).flex().items_center().text_size(px(11.)).text_color(dim).child(label)
        };
        let column = |key: usize, handle: &ScrollHandle, title: &'static str, rows: Vec<gpui::AnyElement>, cx: &mut Context<Self>| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(border)
                .child(header(title))
                .child(self.scroll_area(
                    key,
                    handle,
                    div().id(("debug-column", key)).flex_1().min_h_0().flex().flex_col().overflow_y_scroll().track_scroll(handle).children(rows),
                    cx,
                ))
        };

        // Breakpoints.
        let mut bp_rows = Vec::new();
        for (path, lines) in &self.debug.breakpoints {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for (&line, spec) in lines {
                let (open_path, remove_path) = (path.clone(), path.clone());
                let detail = [
                    spec.condition.as_ref().map(|c| format!("when {c}")),
                    spec.hit_condition.as_ref().map(|h| format!("hit {h}")),
                    spec.log_message.as_ref().map(|m| format!("log {m}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
                bp_rows.push(
                    div()
                        .id(("breakpoint", bp_rows.len()))
                        .h(px(ROW_HEIGHT))
                        .flex_none()
                        .px(px(10.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .hover(move |style| style.bg(hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.open_path_at(&open_path, line, 0, 0, cx)),
                        )
                        .child(div().w(px(8.)).h(px(8.)).flex_none().rounded(px(4.)).bg(theme.debug.breakpoint))
                        .child(format!("{name}:{}", line + 1))
                        .child(div().text_color(dim).text_size(px(12.)).child(detail))
                        .child(
                            div()
                                .id(("breakpoint-remove", bp_rows.len()))
                                .ml_auto()
                                .text_color(dim)
                                .cursor_pointer()
                                .child("\u{00d7}")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        this.toggle_breakpoint_at(&remove_path, line, cx);
                                    }),
                                ),
                        )
                        .into_any_element(),
                );
            }
        }

        // Call stack.
        let frame_rows: Vec<gpui::AnyElement> = self
            .debug
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let selected = index == self.debug.selected_frame;
                let place = frame
                    .path
                    .as_ref()
                    .map(|p| format!("{}:{}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), frame.line + 1))
                    .unwrap_or_default();
                div()
                    .id(("frame", index))
                    .h(px(ROW_HEIGHT))
                    .flex_none()
                    .px(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when(selected, |row| row.bg(theme.lists.active_selection))
                    .hover(move |style| style.bg(hover))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        this.show_frame(index, true, cx);
                        cx.notify();
                    }))
                    .child(frame.name.clone())
                    .child(div().text_color(dim).text_size(px(12.)).child(place))
                    .into_any_element()
            })
            .collect();

        // Watches, then the variables of the frame.
        let mut variable_rows: Vec<gpui::AnyElement> = Vec::new();
        for (index, expression) in self.debug.watches.iter().enumerate() {
            let shown = match self.debug.watch_values.get(expression) {
                Some(Ok(value)) => (format!("= {value}"), text),
                Some(Err(message)) => (format!("{message}"), theme.markers.error),
                None => (if stopped { "...".to_owned() } else { "not available".to_owned() }, dim),
            };
            variable_rows.push(
                div()
                    .id(("watch", index))
                    .h(px(ROW_HEIGHT))
                    .flex_none()
                    .px(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(div().text_color(theme.markers.info).child("\u{25C9}"))
                    .child(expression.clone())
                    .child(div().text_color(shown.1).child(shown.0))
                    .child(
                        div()
                            .id(("watch-remove", index))
                            .ml_auto()
                            .text_color(dim)
                            .cursor_pointer()
                            .child("\u{00d7}")
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.remove_watch(index, cx))),
                    )
                    .into_any_element(),
            );
        }
        let watch_count = variable_rows.len();
        variable_rows.extend(self.variable_rows().into_iter().enumerate().map(|(index, row)| {
            let expandable = row.reference > 0;
            let reference = row.reference;
            div()
                .id(("variable", index + watch_count))
                .h(px(ROW_HEIGHT))
                .flex_none()
                .pl(px(10. + 14. * row.depth as f32))
                .pr(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .whitespace_nowrap()
                .overflow_hidden()
                .when(expandable, |r| r.cursor_pointer().hover(move |style| style.bg(hover)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    if reference > 0 {
                        this.toggle_variable(reference, cx);
                    }
                }))
                .child(div().w(px(12.)).flex_none().child(if expandable {
                    crate::icons::icon(if row.open { "chevron-down" } else { "chevron-right" }, 12., dim).into_any_element()
                } else {
                    div().into_any_element()
                }))
                .child(div().child(row.name))
                .children(row.value.map(|v| div().text_color(text).child(format!("= {v}"))))
                .children(row.kind.map(|k| div().text_color(dim).text_size(px(12.)).child(k)))
                .into_any_element()
        }));

        // Console.
        let console_rows: Vec<gpui::AnyElement> = self
            .debug
            .console
            .iter()
            .enumerate()
            .map(|(index, (category, line))| {
                let color = match category.as_str() {
                    "stderr" => theme.markers.error,
                    "console" => dim,
                    _ => text,
                };
                div().id(("console-line", index)).min_h(px(ROW_HEIGHT)).flex_none().px(px(10.)).text_color(color).child(line.clone()).into_any_element()
            })
            .collect();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(text)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(column(40, &self.debug.scroll_breakpoints, "BREAKPOINTS", bp_rows, cx))
                    .child(column(41, &self.debug.scroll_frames, "CALL STACK", frame_rows, cx))
                    .child(column(42, &self.debug.scroll_variables, "WATCH AND VARIABLES", variable_rows, cx))
                    .child(column(43, &self.debug.scroll_console, "DEBUG CONSOLE", console_rows, cx)),
            )
    }

    /// The bar that floats over the editor while a session runs: carry on or pause, the three
    /// steps, restart and stop. Its grip can be dragged sideways.
    pub(crate) fn render_debug_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let phase = self.debug.phase;
        if phase == DebugPhase::Idle {
            return None;
        }
        let theme = &self.theme;
        let (hover, dim, text) = (theme.lists.hover, theme.gutter_text, theme.text);
        let stopped = phase == DebugPhase::Stopped;
        let button = |id: &'static str, icon: &'static str, tip: &'static str, enabled: bool, color: gpui::Rgba, run: fn(&mut EditorView, &mut Context<EditorView>)| {
            div()
                .id(id)
                .w(px(28.))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .when(enabled, |b| b.cursor_pointer().hover(move |style| style.bg(hover)))
                .tooltip(move |_window, cx| cx.new(|_| Tip(tip)).into())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        if enabled {
                            run(this, cx);
                        }
                    }),
                )
                .child(crate::icons::icon(icon, 16., if enabled { color } else { dim }))
        };
        let blue = gpui::rgb(0x3794ff);
        let green = gpui::rgb(0x89d185);
        let red = theme.debug.breakpoint;
        let bar = div()
            .id("debug-bar")
            .relative()
            .left(px(self.debug.bar_dx))
            .h(px(32.))
            .px(px(4.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .rounded(px(6.))
            .bg(theme.widget_background)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_color(text)
            .occlude()
            .child(
                div()
                    .id("debug-bar-grip")
                    .w(px(14.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_grab()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.debug.bar_grab = Some(f32::from(event.position.x) - this.debug.bar_dx);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _w, cx| {
                        this.debug.bar_grab = None;
                        cx.notify();
                    }))
                    .child(crate::icons::icon("debug-grip", 12., dim)),
            )
            .child(if stopped {
                button("bar-continue", "debug-continue", "Continue (F5)", true, green, |this, cx| this.debug_command(DebugCommand::Continue, cx))
            } else {
                button("bar-pause", "debug-pause", "Pause (F6)", phase == DebugPhase::Running, blue, |this, cx| this.debug_command(DebugCommand::Pause, cx))
            })
            .child(button("bar-over", "debug-step-over", "Step Over (F10)", stopped, blue, |this, cx| this.debug_command(DebugCommand::Next, cx)))
            .child(button("bar-into", "debug-step-into", "Step Into (F11)", stopped, blue, |this, cx| this.debug_command(DebugCommand::StepIn, cx)))
            .child(button("bar-out", "debug-step-out", "Step Out (Shift-F11)", stopped, blue, |this, cx| this.debug_command(DebugCommand::StepOut, cx)))
            .child(button("bar-restart", "debug-restart", "Restart (Cmd-Shift-F5)", true, green, |this, cx| this.restart_debugging(cx)))
            .child(button("bar-stop", "debug-stop", "Stop (Shift-F5)", true, red, |this, cx| this.stop_debugging(cx)));
        Some(
            div()
                .absolute()
                .top(px(6.))
                .left_0()
                .w_full()
                .flex()
                .flex_row()
                .justify_center()
                .child(bar)
                .into_any_element(),
        )
    }
}

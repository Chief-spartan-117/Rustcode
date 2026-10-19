//! Status bar and small helpers shared by the workbench.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent};

use crate::editor_view::EditorView;
use crate::text_layout::line_height;

pub(crate) const STATUS_BAR_HEIGHT: f32 = 24.0;

/// `~/x` becomes the home directory path; relative paths resolve against the working directory.
pub(crate) fn resolve_path(input: &str) -> PathBuf {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    let path = Path::new(input);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

impl EditorView {
    pub(crate) fn cursor_position_label(&self) -> String {
        let editor = &self.document.editor;
        let buffer = editor.buffer();
        let head = editor.selections().last().map_or(0, |s| s.head);
        let line = buffer.char_to_line(head);
        let column = head - buffer.line_to_char(line);
        let mut label = format!("Ln {}, Col {}", line + 1, column + 1);
        let count = editor.selections().len();
        if count > 1 {
            label.push_str(&format!(" ({count} selections)"));
        } else if let Some(selection) = editor.selections().first().filter(|s| !s.is_empty()) {
            label.push_str(&format!(" ({} selected)", selection.end() - selection.start()));
        }
        label
    }

    pub(crate) fn render_status_bar(&self, line_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let language = self.language_label();
        let plain = |text: String| div().px(px(8.)).child(text);
        div()
            .h(px(STATUS_BAR_HEIGHT.max(line_height())))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(4.))
            .bg(theme.status_bar.background)
            .border_t_1()
            .border_color(theme.status_bar.border)
            .text_color(theme.status_bar.foreground)
            .text_size(px(12.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .child({
                        let (errors, warnings) = self.problem_counts();
                        let hover = theme.status_bar.hover;
                        div()
                            .id("status-problems")
                            .px(px(8.))
                            .rounded(px(3.))
                            .hover(move |style| style.bg(hover))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _w, cx| this.show_problems(cx)),
                            )
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(crate::icons::icon("error", 12., crate::problems::severity_color(theme, rustcode_application::Severity::Error)))
                            .child(errors.to_string())
                            .child(crate::icons::icon("warning", 12., crate::problems::severity_color(theme, rustcode_application::Severity::Warning)))
                            .child(warnings.to_string())
                    })
                    .children(self.debug_status_label().map(|label| div().px(px(8.)).text_color(theme.debug.stack_arrow).child(label)))
                    .children(self.render_scm_status(cx))
                    .children(self.ext_status_items.iter().filter(|i| i.left).map(|i| self.render_ext_status_item(i, cx)))
                    .child(plain(self.status.clone().or_else(|| self.unmatched_bracket_hint().map(str::to_owned)).unwrap_or_default())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .children(self.git_state.branch().map(|b| {
                        let hover = theme.status_bar.hover;
                        div()
                            .id("status-branch")
                            .px(px(8.))
                            .rounded(px(3.))
                            .hover(move |style| style.bg(hover))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _w, cx| this.open_branch_picker(cx)),
                            )
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(5.))
                            .child(crate::icons::icon("branch", 12., theme.status_bar.foreground))
                            .child(match self.git_state.status.as_ref() {
                                Some(s) if s.ahead + s.behind > 0 => format!("{b} ↓{} ↑{}", s.behind, s.ahead),
                                _ => b.to_owned(),
                            })
                    }))
                    .children(self.ext_status_items.iter().filter(|i| !i.left).rev().map(|i| self.render_ext_status_item(i, cx)))
                    .children(self.render_flutter_status(cx))
                    .child(self.status_button("status-position", self.cursor_position_label(), |this, cx| this.status_go_to_line(cx), cx))
                    .child(self.status_button("status-lines", format!("{line_count} lines"), |this, cx| this.open_symbol_picker(cx), cx))
                    .child(self.status_button("status-indent", self.indentation_label(), |this, cx| this.pick_indentation(cx), cx))
                    .child(self.status_button("status-eol", self.line_ending_label().to_owned(), |this, cx| this.pick_line_ending(cx), cx))
                    .child(self.status_button("status-language", language, |this, cx| this.pick_language(cx), cx))
                    .child(self.status_button("status-theme", self.themes[self.theme_index].name.clone(), |this, cx| this.open_palette(crate::palette::PaletteMode::Themes, cx), cx)),
            )
    }

    /// An item of the status bar that does something when it is clicked.
    fn status_button(&self, id: &'static str, text: String, action: fn(&mut EditorView, &mut Context<EditorView>), cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let hover = self.theme.status_bar.hover;
        div()
            .id(id)
            .px(px(8.))
            .rounded(px(3.))
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    action(this, cx)
                }),
            )
            .child(text)
    }

    /// An item an extension put in the status bar. A click runs its command.
    fn render_ext_status_item(&self, item: &crate::extensions::ExtStatusItem, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hover = self.theme.status_bar.hover;
        let (command, arguments) = (item.command.clone(), item.arguments.clone());
        div()
            .id(("status-ext", item.id as usize))
            .px(px(8.))
            .rounded(px(3.))
            .whitespace_nowrap()
            .when(command.is_some(), |d| {
                d.cursor_pointer().hover(move |style| style.bg(hover)).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        if let Some(command) = &command {
                            let folders: Vec<std::path::PathBuf> = this.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
                            this.ext_host.run_command(command, &arguments, &folders);
                            cx.notify();
                        }
                    }),
                )
            })
            .child(item.text.clone())
            .into_any_element()
    }
}

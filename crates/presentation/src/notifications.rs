//! Messages that pop up over the window, such as what extensions report, until they are
//! dismissed or have been up long enough.

use std::time::{Duration, Instant};

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent};

use crate::editor_view::EditorView;

/// At most this many are shown; the oldest goes first.
const MAX_TOASTS: usize = 4;
const WIDTH: f32 = 380.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastLevel {
    Info,
    Warning,
    Error,
}

impl ToastLevel {
    /// How long it stays: errors are the ones to read.
    fn lifetime(self) -> Duration {
        Duration::from_secs(match self {
            ToastLevel::Info => 6,
            ToastLevel::Warning => 10,
            ToastLevel::Error => 15,
        })
    }
}

pub(crate) struct Toast {
    pub id: u64,
    pub level: ToastLevel,
    pub text: String,
    created: Instant,
    /// A question of an extension, answered with the button pressed (or dismissed).
    pub request: Option<u64>,
    pub buttons: Vec<String>,
    /// Stays until taken down: a message with buttons waits longer, and progress until it is done.
    pub sticky: bool,
}

impl EditorView {
    /// Show a message over the window.
    pub(crate) fn notify_user(&mut self, level: ToastLevel, text: String, cx: &mut Context<Self>) {
        self.push_toast(level, text, None, Vec::new(), false, cx);
    }

    /// A message with buttons: the extension that asked is answered with the one pressed.
    pub(crate) fn notify_with_buttons(&mut self, level: ToastLevel, text: String, request: u64, buttons: Vec<String>, cx: &mut Context<Self>) {
        self.push_toast(level, text, Some(request), buttons, false, cx);
    }

    /// A message that stays until taken down (progress); returns its number.
    pub(crate) fn notify_sticky(&mut self, level: ToastLevel, text: String, cx: &mut Context<Self>) -> u64 {
        self.push_toast(level, text, None, Vec::new(), true, cx)
    }

    pub(crate) fn update_toast_text(&mut self, id: u64, text: String, cx: &mut Context<Self>) {
        if let Some(toast) = self.toasts.iter_mut().find(|t| t.id == id) {
            toast.text = text;
            cx.notify();
        }
    }

    fn push_toast(&mut self, level: ToastLevel, text: String, request: Option<u64>, buttons: Vec<String>, sticky: bool, cx: &mut Context<Self>) -> u64 {
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(Toast { id, level, text, created: Instant::now(), request, buttons, sticky });
        while self.toasts.len() > MAX_TOASTS {
            // The one pushed out is dismissed: whoever asked is told.
            let gone = self.toasts.remove(0);
            self.toast_gone(&gone);
        }
        cx.notify();
        id
    }

    /// A message that was waiting for an answer went away without one.
    fn toast_gone(&self, toast: &Toast) {
        if let Some(request) = toast.request {
            self.ext_host.prompt_answer(request, rustcode_application::PromptAnswer::Cancelled);
        }
    }

    /// A button of message `id` was pressed.
    pub(crate) fn toast_button(&mut self, id: u64, index: usize, cx: &mut Context<Self>) {
        if let Some(at) = self.toasts.iter().position(|t| t.id == id) {
            let toast = self.toasts.remove(at);
            if let Some(request) = toast.request {
                self.ext_host.prompt_answer(request, rustcode_application::PromptAnswer::Button(index));
            }
            cx.notify();
        }
    }

    /// Take down the messages that have been up long enough as of `now`.
    pub(crate) fn prune_notifications_at(&mut self, now: Instant, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        // A question waits a good while for its answer; progress, until it is done.
        let alive = |t: &Toast| {
            let age = now.duration_since(t.created);
            if t.sticky {
                true
            } else if !t.buttons.is_empty() {
                age < Duration::from_secs(120)
            } else {
                age < t.level.lifetime()
            }
        };
        let (keep, gone): (Vec<Toast>, Vec<Toast>) = std::mem::take(&mut self.toasts).into_iter().partition(|t| alive(t));
        self.toasts = keep;
        for toast in &gone {
            self.toast_gone(toast);
        }
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    pub(crate) fn dismiss_notification(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(at) = self.toasts.iter().position(|t| t.id == id) {
            let toast = self.toasts.remove(at);
            self.toast_gone(&toast);
        }
        cx.notify();
    }

    pub(crate) fn render_notifications(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.toasts.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let colors = &theme.notifications;
        let hover = theme.lists.hover;
        let toasts = self.toasts.iter().map(|toast| {
            let id = toast.id;
            let (icon, color) = match toast.level {
                ToastLevel::Info => ("info", colors.info_icon),
                ToastLevel::Warning => ("warning", colors.warning_icon),
                ToastLevel::Error => ("error", colors.error_icon),
            };
            div()
                .id(("toast", id as usize))
                .w(px(WIDTH))
                .p(px(10.))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.background)
                .text_color(colors.foreground)
                .text_size(px(13.))
                .shadow_md()
                .occlude()
                .child(div().flex_none().pt(px(1.)).child(crate::icons::icon(icon, 14., color)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(toast.text.clone())
                        .children((!toast.buttons.is_empty()).then(|| {
                            div().flex().flex_row().flex_wrap().gap(px(6.)).pt(px(2.)).children(toast.buttons.iter().enumerate().map(|(index, title)| {
                                div()
                                    .id(("toast-button", id as usize * 100 + index))
                                    .px(px(10.))
                                    .py(px(3.))
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .bg(if index == 0 { theme.button.background } else { theme.widget_background })
                                    .text_color(if index == 0 { theme.button.foreground } else { theme.text })
                                    .border_1()
                                    .border_color(colors.border)
                                    .text_size(px(12.))
                                    .hover(|style| style.opacity(0.85))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                            cx.stop_propagation();
                                            this.toast_button(id, index, cx)
                                        }),
                                    )
                                    .child(title.clone())
                            }))
                        }))
                        // An error has more to say in the Output tab.
                        .children((toast.level == ToastLevel::Error).then(|| {
                            div()
                                .id(("toast-details", id as usize))
                                .text_size(px(12.))
                                .text_color(colors.link)
                                .underline()
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        this.show_panel_tab(crate::shell::PanelTab::Output, cx);
                                        this.dismiss_notification(id, cx)
                                    }),
                                )
                                .child("Show Output")
                        })),
                )
                .child(
                    div()
                        .id(("toast-close", id as usize))
                        .flex_none()
                        .rounded(px(3.))
                        .hover(move |style| style.bg(hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.dismiss_notification(id, cx)
                            }),
                        )
                        .child(crate::icons::icon("close", 12., colors.foreground)),
                )
        });
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(px(crate::workbench::STATUS_BAR_HEIGHT + 12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(toasts)
                .child(self.overlay_probe()),
        )
    }
}

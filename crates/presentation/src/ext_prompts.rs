//! Questions extensions ask: pick from a list, type a line, messages with buttons, files to open
//! or save, and progress in a notification.

use std::collections::BTreeSet;
use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent};
use rustcode_application::{HostEvent, MessageLevel, PickItem, PromptAnswer, PromptKind};

use crate::editor_view::EditorView;
use crate::notifications::ToastLevel;
use crate::palette::{PaletteAction, PaletteItem, PaletteMode};

/// The list or the line an extension is asking for, while the palette shows it.
pub(crate) struct ExtPrompt {
    pub request: u64,
    pub kind: ExtPromptKind,
    /// The extension refused what was typed: why. Shown until the text changes.
    pub invalid: Option<String>,
}

pub(crate) enum ExtPromptKind {
    Pick { items: Vec<PickItem>, many: bool, placeholder: Option<String>, match_description: bool, match_detail: bool, chosen: BTreeSet<usize> },
    Input { prompt: Option<String>, placeholder: Option<String>, password: bool },
}

/// A message that must be answered before going on (`{ modal: true }`).
pub(crate) struct ExtDialog {
    pub request: u64,
    pub level: MessageLevel,
    pub text: String,
    pub detail: Option<String>,
    pub buttons: Vec<String>,
}

fn toast_level(level: MessageLevel) -> ToastLevel {
    match level {
        MessageLevel::Info => ToastLevel::Info,
        MessageLevel::Warning => ToastLevel::Warning,
        MessageLevel::Error => ToastLevel::Error,
    }
}

impl EditorView {
    /// A question from an extension: show it.
    pub(crate) fn extension_prompt(&mut self, request: u64, title: Option<String>, kind: PromptKind, cx: &mut Context<Self>) {
        match kind {
            PromptKind::Pick { items, many, placeholder, match_description, match_detail } => {
                let chosen = items.iter().enumerate().filter(|(_, i)| many && i.picked).map(|(n, _)| n).collect();
                self.ext_prompt = Some(ExtPrompt {
                    request,
                    kind: ExtPromptKind::Pick { items, many, placeholder, match_description, match_detail, chosen },
                    invalid: None,
                });
                self.open_palette(PaletteMode::ExtPick, cx);
                if let Some(title) = title {
                    self.set_status(title);
                }
            }
            PromptKind::Input { prompt, placeholder, value, password } => {
                self.ext_prompt = Some(ExtPrompt { request, kind: ExtPromptKind::Input { prompt: prompt.or(title), placeholder, password }, invalid: None });
                self.open_prompt(PaletteMode::ExtInput, &value, None, cx);
                self.palette.query.select_all();
            }
            PromptKind::Message { level, text, detail, modal, buttons } => {
                if modal {
                    self.ext_dialog = Some(ExtDialog { request, level, text, detail, buttons });
                    cx.notify();
                } else {
                    let text = match detail {
                        Some(detail) => format!("{text}\n{detail}"),
                        None => text,
                    };
                    self.notify_with_buttons(toast_level(level), text, request, buttons, cx);
                }
            }
            PromptKind::OpenDialog { files, folders, many, .. } => {
                let choice = cx.prompt_for_paths(gpui::PathPromptOptions { files, directories: folders, multiple: many, prompt: title.map(Into::into) });
                cx.spawn(async move |this, cx| {
                    let answer = match choice.await {
                        Ok(Ok(Some(paths))) => PromptAnswer::Paths(paths),
                        _ => PromptAnswer::Cancelled,
                    };
                    this.update(cx, |this, _| this.ext_host.prompt_answer(request, answer)).ok();
                })
                .detach();
            }
            PromptKind::SaveDialog { default, .. } => {
                let directory = default.as_deref().and_then(|p| p.parent()).map(PathBuf::from).or_else(|| self.shell.explorers.first().map(|t| t.root().to_path_buf())).unwrap_or_default();
                let name = default.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                let choice = cx.prompt_for_new_path(&directory, name.as_deref());
                cx.spawn(async move |this, cx| {
                    let answer = match choice.await {
                        Ok(Ok(Some(path))) => PromptAnswer::Paths(vec![path]),
                        _ => PromptAnswer::Cancelled,
                    };
                    this.update(cx, |this, _| this.ext_host.prompt_answer(request, answer)).ok();
                })
                .detach();
            }
        }
    }

    /// The extension refused the text of the input box that is up.
    pub(crate) fn extension_prompt_invalid(&mut self, request: u64, message: String, cx: &mut Context<Self>) {
        if let Some(prompt) = self.ext_prompt.as_mut().filter(|p| p.request == request) {
            prompt.invalid = Some(message);
            cx.notify();
        }
    }

    /// The extension took the text of the input box: close it.
    pub(crate) fn extension_prompt_done(&mut self, request: u64, cx: &mut Context<Self>) {
        if self.ext_prompt.as_ref().is_some_and(|p| p.request == request) {
            self.ext_prompt = None;
            self.close_palette(false, cx);
        }
    }

    /// The palette closed without an answer: tell the extension.
    pub(crate) fn extension_prompt_cancelled(&mut self) {
        if let Some(prompt) = self.ext_prompt.take() {
            self.ext_host.prompt_answer(prompt.request, PromptAnswer::Cancelled);
        }
    }

    /// The rows of the extension's list, or the one row of its input line.
    pub(crate) fn ext_prompt_items(&self, mode: PaletteMode) -> Vec<PaletteItem> {
        let Some(prompt) = self.ext_prompt.as_ref() else { return Vec::new() };
        let query = self.palette.query.text();
        match (&prompt.kind, mode) {
            (ExtPromptKind::Pick { items, many, match_description, match_detail, chosen, .. }, PaletteMode::ExtPick) => {
                let typed = query.trim().to_lowercase();
                let fits = |item: &PickItem| {
                    typed.is_empty()
                        || item.label.to_lowercase().contains(&typed)
                        || (*match_description && item.description.as_ref().is_some_and(|d| d.to_lowercase().contains(&typed)))
                        || (*match_detail && item.detail.as_ref().is_some_and(|d| d.to_lowercase().contains(&typed)))
                };
                let mut rows: Vec<PaletteItem> = Vec::new();
                if *many {
                    rows.push(PaletteItem {
                        label: format!("Accept {} selected", chosen.len()),
                        hint: Some("choose these".to_owned()),
                        action: PaletteAction::ExtPickDone,
                    });
                }
                rows.extend(items.iter().enumerate().filter(|(_, item)| fits(item)).map(|(index, item)| {
                    let hint = [item.description.as_deref(), item.detail.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("  ");
                    let label = if *many {
                        format!("{} {}", if chosen.contains(&index) { "\u{2611}" } else { "\u{2610}" }, item.label)
                    } else {
                        item.label.clone()
                    };
                    PaletteItem {
                        label,
                        hint: (!hint.is_empty()).then_some(hint),
                        action: if *many { PaletteAction::ExtToggle(index) } else { PaletteAction::ExtPick(index) },
                    }
                }));
                rows
            }
            (ExtPromptKind::Input { prompt: question, password, .. }, PaletteMode::ExtInput) => {
                let shown = if *password { "\u{2022}".repeat(query.chars().count()) } else { query.clone() };
                let label = match (&prompt.invalid, question) {
                    (Some(problem), _) => format!("\u{26a0} {problem}"),
                    (None, Some(question)) => format!("{question}: {shown}"),
                    (None, None) => format!("Press Enter to confirm: {shown}"),
                };
                vec![PaletteItem { label, hint: None, action: PaletteAction::ExtInput(query) }]
            }
            _ => Vec::new(),
        }
    }

    /// What the list or line is for when nothing matches.
    pub(crate) fn ext_prompt_empty_text(&self) -> String {
        match self.ext_prompt.as_ref().map(|p| &p.kind) {
            Some(ExtPromptKind::Pick { placeholder, .. }) => placeholder.clone().unwrap_or_else(|| "No matching items".to_owned()),
            Some(ExtPromptKind::Input { placeholder, prompt, .. }) => placeholder.clone().or_else(|| prompt.clone()).unwrap_or_else(|| "Type a value".to_owned()),
            None => "No matching items".to_owned(),
        }
    }

    /// Whether the input line of the extension's prompt is a password.
    pub(crate) fn ext_prompt_is_password(&self) -> bool {
        matches!(self.ext_prompt.as_ref().map(|p| &p.kind), Some(ExtPromptKind::Input { password: true, .. }))
    }

    /// A row of the extension's list was taken.
    pub(crate) fn ext_prompt_accept(&mut self, action: PaletteAction, cx: &mut Context<Self>) {
        let Some(request) = self.ext_prompt.as_ref().map(|p| p.request) else { return };
        match action {
            PaletteAction::ExtToggle(index) => {
                if let Some(ExtPromptKind::Pick { chosen, .. }) = self.ext_prompt.as_mut().map(|p| &mut p.kind) {
                    if !chosen.remove(&index) {
                        chosen.insert(index);
                    }
                }
                cx.notify();
            }
            PaletteAction::ExtPick(index) => {
                self.ext_prompt = None;
                self.close_palette(false, cx);
                self.ext_host.prompt_answer(request, PromptAnswer::Picked(vec![index]));
            }
            PaletteAction::ExtPickDone => {
                let picked = match self.ext_prompt.as_ref().map(|p| &p.kind) {
                    Some(ExtPromptKind::Pick { chosen, .. }) => chosen.iter().copied().collect(),
                    _ => Vec::new(),
                };
                self.ext_prompt = None;
                self.close_palette(false, cx);
                self.ext_host.prompt_answer(request, PromptAnswer::Picked(picked));
            }
            PaletteAction::ExtInput(text) => {
                // The prompt stays until the extension says the text is fine (it may refuse it).
                if let Some(prompt) = self.ext_prompt.as_mut() {
                    prompt.invalid = None;
                }
                self.ext_host.prompt_answer(request, PromptAnswer::Text(text));
            }
            _ => {}
        }
    }

    /// A button of the dialog was pressed (`None`: it was dismissed).
    pub(crate) fn ext_dialog_answer(&mut self, button: Option<usize>, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ext_dialog.take() {
            self.ext_host.prompt_answer(dialog.request, button.map_or(PromptAnswer::Cancelled, PromptAnswer::Button));
            cx.notify();
        }
    }

    pub(crate) fn render_ext_dialog(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let dialog = self.ext_dialog.as_ref()?;
        let theme = &self.theme;
        let colors = &theme.notifications;
        let (icon, color) = match dialog.level {
            MessageLevel::Info => ("info", colors.info_icon),
            MessageLevel::Warning => ("warning", colors.warning_icon),
            MessageLevel::Error => ("error", colors.error_icon),
        };
        let buttons = dialog.buttons.iter().enumerate().map(|(index, title)| {
            div()
                .id(("dialog-button", index))
                .px(px(14.))
                .py(px(5.))
                .rounded(px(3.))
                .cursor_pointer()
                .bg(if index == 0 { theme.button.background } else { theme.widget_background })
                .text_color(if index == 0 { theme.button.foreground } else { theme.text })
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.opacity(0.85))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.ext_dialog_answer(Some(index), cx)
                    }),
                )
                .child(title.clone())
        });
        Some(
            div()
                .id("ext-dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000066))
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _w, cx| this.ext_dialog_answer(None, cx)))
                .child(
                    div()
                        .id("ext-dialog")
                        .w(px(440.))
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.background)
                        .text_color(colors.foreground)
                        .text_size(px(13.))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(10.))
                                .items_start()
                                .child(div().flex_none().child(crate::icons::icon(icon, 20., color)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.))
                                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(dialog.text.clone()))
                                        .children(dialog.detail.clone().map(|d| div().text_color(theme.gutter_text).child(d))),
                                ),
                        )
                        .child(div().flex().flex_row().justify_end().gap(px(8.)).children(buttons))
                        .child(self.overlay_probe()),
                ),
        )
    }

    /// The events about questions, progress and status that `poll_extension_host` hands over.
    pub(crate) fn extension_progress(&mut self, id: u64, text: String, done: bool, cx: &mut Context<Self>) {
        if done {
            if let Some(toast) = self.ext_progress.remove(&id) {
                self.dismiss_notification(toast, cx);
            }
            return;
        }
        match self.ext_progress.get(&id).copied() {
            Some(toast) => self.update_toast_text(toast, text, cx),
            None => {
                let toast = self.notify_sticky(ToastLevel::Info, text, cx);
                self.ext_progress.insert(id, toast);
            }
        }
    }
}

/// Which `HostEvent`s are questions.
#[allow(dead_code)]
pub(crate) fn is_question(event: &HostEvent) -> bool {
    matches!(event, HostEvent::Prompt { .. } | HostEvent::PromptInvalid { .. })
}

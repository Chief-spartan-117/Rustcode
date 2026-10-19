//! What the editor area shows when no file is open.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, MouseButton, MouseDownEvent};

use crate::editor_view::EditorView;
use crate::palette::ids;

/// Modifier named the way the platform names it.
fn modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

impl EditorView {
    /// Whether the only thing open is the placeholder document left when the last tab was closed.
    pub(crate) fn is_empty_state(&self) -> bool {
        let Some(placeholder) = self.placeholder_doc else { return false };
        self.groups.len() == 1
            && self.groups[0].tabs.len() == 1
            && self.groups[0].tabs[0].doc == placeholder
            && !self.doc_label(placeholder).1
    }

    /// Show the empty state: the document in the window now stands in for "nothing open".
    pub fn show_empty_state(&mut self) {
        self.placeholder_doc = Some(self.document.id);
    }

    pub(crate) fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (text, dim, border, hover, fill) =
            (theme.text, theme.gutter_text, theme.border, theme.selection, theme.widget_background);
        let m = modifier();
        let action = |id: &'static str, icon: &'static str, label: &'static str, keys: String, run: fn(&mut EditorView, &mut Context<EditorView>), cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(300.))
                .h(px(36.))
                .px(px(14.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .rounded(px(4.))
                .border_1()
                .border_color(border)
                .bg(fill)
                .text_color(text)
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| run(this, cx)),
                )
                .child(div().flex().flex_row().items_center().gap(px(10.)).child(crate::icons::icon(icon, 16., dim)).child(label))
                .child(div().text_color(dim).text_size(px(12.)).child(keys))
        };
        div()
            .id("empty-state")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .bg(theme.background)
            .child(div().text_size(px(34.)).font_weight(FontWeight::BOLD).text_color(dim).child("RustCode"))
            .child(div().mb(px(14.)).text_color(dim).child("A code editor in Rust, without Electron"))
            .child(action("empty-open-file", "file-text", "Open File...", format!("{m}O"), |v, cx| v.run_command(ids::OPEN_FILE, cx), cx))
            .child(action(
                "empty-open-folder",
                "folder-open",
                "Open Folder...",
                format!("{m}K {m}O"),
                |v, cx| v.open_folder_dialog(cx),
                cx,
            ))
            .child(action("empty-new-file", "file-plus", "New File", format!("{m}N"), |v, cx| v.run_command(ids::NEW_FILE, cx), cx))
            .child(action(
                "empty-palette",
                "command",
                "Command Palette",
                format!("{m}⇧P"),
                |v, cx| v.run_command(ids::COMMAND_PALETTE, cx),
                cx,
            ))
            .child(action("empty-settings", "gear", "Settings", format!("{m},"), |v, cx| v.run_command(ids::OPEN_SETTINGS_PAGE, cx), cx))
            .child(action(
                "empty-shortcuts",
                "keyboard",
                "Keyboard Shortcuts",
                format!("{m}K {m}S"),
                |v, cx| v.run_command(ids::OPEN_SHORTCUTS_PAGE, cx),
                cx,
            ))
            .into_any_element()
    }
}

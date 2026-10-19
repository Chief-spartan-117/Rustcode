//! The project chip in the title bar and the list it opens: the folders of this window, the
//! projects opened before, and a way to open another one.

use std::path::{Path, PathBuf};

use gpui::{canvas, deferred, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, Window};

use crate::editor_view::{EditorView, Focus};
use crate::palette::{PaletteAction, PaletteItem, PaletteMode};

/// Rows of the project list that show at once.
const SWITCHER_ROWS: usize = 14;
const SWITCHER_WIDTH: f32 = 360.0;
const ROW_HEIGHT: f32 = 30.0;

fn folder_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

impl EditorView {
    /// The folder of this window the open file belongs to (the first one when none does).
    pub(crate) fn active_root_index(&self) -> usize {
        if let Some((index, doc)) = self.switched_root {
            if doc == self.document.id && index < self.shell.explorers.len() {
                return index;
            }
        }
        let Some(path) = self.doc_path(self.document.id) else { return 0 };
        self.shell.explorers.iter().position(|tree| path.starts_with(tree.root())).unwrap_or(0)
    }

    /// The name the chip shows.
    pub(crate) fn project_name(&self) -> String {
        match self.shell.explorers.get(self.active_root_index()) {
            Some(tree) => folder_name(tree.root()),
            None => "RustCode".to_owned(),
        }
    }

    /// Folders opened before and not open now, latest first.
    fn recent_projects(&self) -> Vec<PathBuf> {
        let open: Vec<&Path> = self.shell.explorers.iter().map(|t| t.root()).collect();
        self.session_store.recent_roots().into_iter().filter(|root| !open.contains(&root.as_path())).collect()
    }

    /// Entries of the list, with the headers that name the groups (an entry only shows when the
    /// text typed is in its name).
    pub(crate) fn project_items(&self, query: &str) -> Vec<PaletteItem> {
        let wanted = query.to_lowercase();
        let matches = |name: &str| wanted.is_empty() || name.to_lowercase().contains(&wanted);
        let current = self.active_root_index();
        let mut items = Vec::new();

        // "This Window" holds the folders of the window; the active one is marked.
        let open: Vec<PaletteItem> = self
            .shell
            .explorers
            .iter()
            .enumerate()
            .filter(|(_, tree)| matches(&folder_name(tree.root())))
            .map(|(index, tree)| PaletteItem {
                label: folder_name(tree.root()),
                hint: Some(if index == current { "current".to_owned() } else { tree.root().display().to_string() }),
                action: PaletteAction::SwitchRoot(index),
            })
            .collect();
        if !open.is_empty() {
            items.push(PaletteItem { label: "This Window".to_owned(), hint: None, action: PaletteAction::Header });
            items.extend(open);
        }
        let recent: Vec<PaletteItem> = self
            .recent_projects()
            .into_iter()
            .filter(|root| matches(&folder_name(root)))
            .map(|root| PaletteItem { label: folder_name(&root), hint: Some(root.display().to_string()), action: PaletteAction::OpenRecent(root) })
            .collect();
        if !recent.is_empty() {
            items.push(PaletteItem { label: "Recent Projects".to_owned(), hint: None, action: PaletteAction::Header });
            items.extend(recent);
        }
        items.push(PaletteItem { label: "Open Folder...".to_owned(), hint: None, action: PaletteAction::OpenLocalFolders });
        items.push(PaletteItem { label: "Add Folder to Workspace...".to_owned(), hint: None, action: PaletteAction::AddLocalFolder });
        items
    }

    /// Show a folder of this window: the explorer opens at it.
    pub(crate) fn switch_to_root(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(root) = self.shell.explorers.get(index).map(|t| t.root().to_path_buf()) else { return };
        self.show_side_view(crate::shell::SideView::Explorer, cx);
        self.switched_root = Some((index, self.document.id));
        self.shell.cursor = Some(root);
        cx.notify();
    }

    /// The chip in the title bar.
    pub(crate) fn render_project_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let open = self.palette.open && self.palette.mode == PaletteMode::Projects;
        let (hover, text) = (theme.lists.hover, theme.title_bar_text);
        let cell = self.chip_bounds.clone();
        div()
            .id("project-chip")
            .relative()
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .flex_none()
            .rounded(px(4.))
            .when(open, |chip| chip.bg(hover))
            .text_color(if open { theme.focus_border } else { text })
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    if this.palette.open && this.palette.mode == PaletteMode::Projects {
                        this.close_palette(true, cx);
                    } else {
                        this.open_palette(PaletteMode::Projects, cx);
                    }
                }),
            )
            .child(canvas(|bounds, _, _| bounds, move |bounds, _, _, _| cell.set(bounds)).absolute().size_full())
            .child(self.project_name())
    }

    /// The list under the chip.
    pub(crate) fn render_project_switcher(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let items = self.palette_items();
        let selected = self.palette.selected.min(items.len().saturating_sub(1));
        let first = (selected + 1).saturating_sub(SWITCHER_ROWS);
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.quick_input.foreground, theme.gutter_text);
        let chip = self.chip_bounds.get();
        let left = f32::from(chip.origin.x).max(8.0);
        let top = f32::from(chip.origin.y + chip.size.height) + 6.0;

        let open_roots = self.shell.explorers.len();
        let footer_from = items.iter().position(|i| matches!(i.action, PaletteAction::OpenLocalFolders)).unwrap_or(items.len());
        let row = |index: usize, item: &PaletteItem| -> AnyElement {
            if matches!(item.action, PaletteAction::Header) {
                return div()
                    .id(("project-header", index))
                    .h(px(ROW_HEIGHT - 4.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(dim)
                    .child(item.label.clone())
                    .into_any_element();
            }
            let current = matches!(item.action, PaletteAction::SwitchRoot(_)) && item.hint.as_deref() == Some("current");
            let recent = matches!(item.action, PaletteAction::OpenRecent(_));
            let root_index = match item.action {
                PaletteAction::SwitchRoot(i) if open_roots > 1 => Some(i),
                _ => None,
            };
            div()
                .id(("project-row", index))
                .h(px(ROW_HEIGHT))
                .mx(px(4.))
                .px(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .text_color(text)
                .cursor_pointer()
                .when(index == selected, |row| row.bg(hover))
                .hover(move |style| style.bg(hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.palette_accept_at(index, cx)
                    }),
                )
                .child(div().flex_none().child(item.label.clone()))
                .children(current.then(|| crate::icons::icon("check", 14., theme.focus_border)))
                // A folder that is not the one shown says where it is.
                .children((!current && root_index.is_some() || (!current && matches!(item.action, PaletteAction::SwitchRoot(_)))).then(|| item.hint.clone()).flatten().map(|path| {
                    div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(dim).child(path)
                }))
                // A way to take a folder out of this window.
                .children(root_index.map(|index| {
                    div()
                        .id(("project-remove", index))
                        .ml_auto()
                        .flex_none()
                        .w(px(20.))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .hover(move |style| style.bg(gpui::Rgba { a: 0.25, ..dim }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.remove_workspace_folder(index, cx);
                            }),
                        )
                        .child(crate::icons::icon("close", 12., dim))
                }))
                // A recent project shows where it is, dimmed and cut off when long.
                .children(recent.then(|| item.hint.clone()).flatten().map(|path| {
                    div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(dim).child(path)
                }))
                .into_any_element()
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, item) in items.iter().enumerate().skip(first).take(SWITCHER_ROWS) {
            if index == footer_from {
                rows.push(div().h(px(1.)).my(px(4.)).bg(theme.menu.separator).into_any_element());
            }
            rows.push(row(index, item));
        }

        let placeholder = self.palette.query.buffer().len_chars() == 0;
        deferred(
            div()
                .id("project-switcher")
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(SWITCHER_WIDTH))
                .pb(px(4.))
                .flex()
                .flex_col()
                .rounded(px(8.))
                .border_1()
                .border_color(theme.menu.border)
                .bg(theme.quick_input.background)
                .text_color(text)
                .shadow_lg()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()))
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                    // A press on the chip itself is the chip's to handle (it closes the list).
                    let chip = this.chip_bounds.get();
                    if !chip.contains(&event.position) {
                        this.close_palette(true, cx);
                    }
                }))
                .child(
                    div()
                        .relative()
                        .m(px(6.))
                        .child(self.input("palette-input", &self.palette.query, true, Focus::Palette, 2, SWITCHER_WIDTH - 12.0, window, cx))
                        .children(placeholder.then(|| {
                            div().absolute().top(px(3.)).left(px(crate::find_bar::INPUT_PADDING + 1.0)).text_color(theme.input.placeholder).child("Search projects...")
                        })),
                )
                .children(rows)
                .child(self.overlay_probe()),
        )
        .with_priority(30)
        .into_any_element()
    }
}

impl EditorView {
    /// The question that comes with opening a folder when this window has one: a window of its
    /// own, or this one, as the only folder or next to the others.
    pub(crate) fn render_open_choice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let path = self.open_choice.clone()?;
        let theme = &self.theme;
        let colors = &theme.notifications;
        let name = folder_name(&path);
        let option = |id: &'static str, title: &'static str, detail: &'static str, primary: bool, target: PathBuf, kind: u8| {
            div()
                .id(id)
                .px(px(12.))
                .py(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .border_1()
                .border_color(theme.border)
                .bg(if primary { theme.button.background } else { theme.widget_background })
                .text_color(if primary { theme.button.foreground } else { theme.text })
                .hover(|style| style.opacity(0.85))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.open_choice = None;
                        match kind {
                            0 => this.open_in_new_window(&target, cx),
                            1 => this.open_workspace_folder(&target, cx),
                            _ => this.add_workspace_folder(&target, cx),
                        }
                        cx.notify();
                    }),
                )
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(px(12.)).opacity(0.8).child(detail))
        };
        Some(
            div()
                .id("open-choice-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000066))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                        this.open_choice = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("open-choice")
                        .w(px(460.))
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.background)
                        .text_color(colors.foreground)
                        .text_size(px(13.))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()))
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Open \"{name}\"")))
                        .child(div().text_color(theme.gutter_text).child(path.display().to_string()))
                        .child(option("open-choice-new", "New Window", "Keep this window as it is", true, path.clone(), 0))
                        .child(option("open-choice-add", "Add to This Window", "Show it next to the folders already open", false, path.clone(), 2))
                        .child(option("open-choice-replace", "Replace This Window's Folders", "Close the folders open here and show this one", false, path, 1))
                        .child(
                            div()
                                .id("open-choice-cancel")
                                .ml_auto()
                                .px(px(12.))
                                .py(px(4.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .text_color(theme.gutter_text)
                                .hover(|style| style.opacity(0.7))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        this.open_choice = None;
                                        cx.notify();
                                    }),
                                )
                                .child("Cancel"),
                        )
                        .child(self.overlay_probe()),
                ),
        )
    }
}

//! The explorer side bar: tree drawing, context menu, keyboard navigation and file operations.

use std::path::{Path, PathBuf};

use std::collections::HashSet;

use gpui::{
    deferred, div, prelude::*, px, rgb, ClipboardItem, Context, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, Rgba, Window,
};
use rustcode_application::{copy_name, TreeRow};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;
use crate::palette::PaletteMode;
use crate::shell::{
    ContextMenu, ContextTarget, FileClipboard, MAX_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH, ROW_HEIGHT,
};

pub(crate) const CONTEXT_MENU_WIDTH: f32 = 290.0;

/// Explorer rows being dragged.
pub(crate) struct DraggedEntries {
    pub paths: Vec<PathBuf>,
    /// The paths that are files, for dropping on an editor.
    pub files: Vec<PathBuf>,
}

struct EntriesPreview {
    label: String,
    background: Rgba,
    text: Rgba,
}

impl Render for EntriesPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(self.background)
            .text_color(self.text)
            .opacity(0.9)
            .child(self.label.clone())
    }
}

/// What a context menu entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextAction {
    Open(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    CopyPath(PathBuf),
    Cut(PathBuf),
    Copy(PathBuf),
    Paste(PathBuf),
    RemoveRoot(usize),
    Refresh,
    OpenFolder,
    NewUntitled,
    Editor(EditorCommand),
    /// An entry of a notebook cell's menu.
    Notebook(crate::notebook_view::NotebookCommand),
    /// An entry of the output panel's menu.
    Output(OutputAction),
    /// Any command of the palette.
    Command(rustcode_application::CommandId),
    /// An entry of an extension's menu, with the arguments that say what was clicked.
    ExtMenu(crate::shell::ExtMenuItem, Vec<serde_json::Value>),
}

/// What the output panel's right-click menu can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputAction {
    Copy,
    SelectAll,
    Clear,
}

/// What the editor's right-click menu can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorCommand {
    GoToDefinition,
    FindReferences,
    Rename,
    QuickFix,
    Format,
    ToggleComment,
    Cut,
    Copy,
    Paste,
    SelectAll,
    CommandPalette,
}

/// How to draw an icon id from the icon theme: a file path is read from disk, anything else is
/// one of the program's bundled icons.
pub(crate) fn icon_source(id: &str) -> gpui::ImageSource {
    if Path::new(id).is_absolute() {
        PathBuf::from(id).into()
    } else {
        id.to_owned().into()
    }
}

/// Colored dot and tint for a file name, so types are told apart at a glance.
fn file_color(name: &str) -> Rgba {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("rs") => rgb(0xdea584),
        Some("toml" | "yaml" | "yml" | "ini") => rgb(0xb0a070),
        Some("md" | "markdown" | "txt") => rgb(0x519aba),
        Some("json" | "jsonc" | "js" | "mjs") => rgb(0xcbcb41),
        Some("ts" | "tsx" | "css" | "scss") => rgb(0x519aba),
        Some("py") => rgb(0x6aa84f),
        Some("html" | "htm") => rgb(0xe34c26),
        Some("sh" | "zsh" | "bash") => rgb(0x89e051),
        Some("lock") => rgb(0x6d8086),
        Some("png" | "jpg" | "jpeg" | "gif" | "svg") => rgb(0xa074c4),
        _ => rgb(0x8a8a8a),
    }
}

/// A shortcut an entry shows: the action to look up and the key context it is bound in.
pub(crate) type ShortcutOf = (fn() -> Box<dyn gpui::Action>, &'static str);

/// One line of a context menu.
pub(crate) struct ContextEntry {
    pub label: &'static str,
    pub icon: &'static str,
    pub action: ContextAction,
    pub shortcut: Option<ShortcutOf>,
}

pub(crate) enum ContextItem {
    Entry(ContextEntry),
    Separator,
}

/// Entries of the context menu for `target`, in groups.
pub(crate) fn context_entries(target: &ContextTarget) -> Vec<ContextItem> {
    use crate::actions as a;
    use ContextItem::Separator;
    let editor = |label, icon, command, shortcut: Option<ShortcutOf>| {
        ContextItem::Entry(ContextEntry { label, icon, action: ContextAction::Editor(command), shortcut })
    };
    let entry = |label, icon, action, shortcut: Option<ShortcutOf>| ContextItem::Entry(ContextEntry { label, icon, action, shortcut });
    let ed = a::EDITOR_CONTEXT;
    let ex = a::EXPLORER_CONTEXT;
    use EditorCommand as C;
    match target {
        ContextTarget::Extension { .. } => Vec::new(),
        ContextTarget::Editor => vec![
            editor("Go to Definition", "arrow-square-out", C::GoToDefinition, Some((|| Box::new(a::GoToDefinition), ed))),
            editor("Find All References", "search", C::FindReferences, Some((|| Box::new(a::FindReferences), ed))),
            editor("Rename Symbol", "pencil-simple-line", C::Rename, Some((|| Box::new(a::RenameSymbol), ed))),
            editor("Quick Fix...", "lightbulb", C::QuickFix, Some((|| Box::new(a::QuickFix), ed))),
            Separator,
            editor("Format Document", "text-align-left", C::Format, Some((|| Box::new(a::FormatDocument), ed))),
            editor("Toggle Line Comment", "hash", C::ToggleComment, Some((|| Box::new(a::ToggleLineComment), ed))),
            Separator,
            editor("Cut", "scissors", C::Cut, Some((|| Box::new(a::Cut), ed))),
            editor("Copy", "copy", C::Copy, Some((|| Box::new(a::Copy), ed))),
            editor("Paste", "clipboard", C::Paste, Some((|| Box::new(a::Paste), ed))),
            Separator,
            editor("Select All", "selection-all", C::SelectAll, Some((|| Box::new(a::SelectAll), ed))),
            Separator,
            editor("Command Palette...", "command", C::CommandPalette, Some((|| Box::new(a::OpenCommandPalette), a::WORKBENCH_CONTEXT))),
        ],
        // The header of a folder, or the empty part of the explorer: what can be done with the folder.
        ContextTarget::Root(_) | ContextTarget::Blank => {
            let mut items = vec![
                entry("New File...", "file-plus", ContextAction::NewFile(PathBuf::new()), None),
                entry("New Folder...", "folder-plus", ContextAction::NewFolder(PathBuf::new()), None),
                Separator,
                entry("Paste", "clipboard", ContextAction::Paste(PathBuf::new()), Some((|| Box::new(a::ExplorerPaste), ex))),
                entry("Refresh", "refresh", ContextAction::Refresh, None),
            ];
            if let ContextTarget::Root(index) = target {
                items.push(Separator);
                items.push(entry("Remove Folder from Workspace", "close", ContextAction::RemoveRoot(*index), None));
            }
            items
        }
        ContextTarget::NotebookCell => {
            use crate::notebook_view::NotebookCommand as N;
            let nb = |label, icon, command| ContextItem::Entry(ContextEntry { label, icon, action: ContextAction::Notebook(command), shortcut: None });
            vec![
                nb("Cut Cell", "scissors", N::Cut),
                nb("Copy Cell", "copy", N::Copy),
                nb("Paste Cell", "clipboard", N::Paste),
                Separator,
                nb("Insert Code Cell Above", "plus", N::InsertAbove(false)),
                nb("Insert Code Cell Below", "plus", N::InsertBelow(false)),
                nb("Insert Markdown Cell Above", "plus", N::InsertAbove(true)),
                nb("Insert Markdown Cell Below", "plus", N::InsertBelow(true)),
                Separator,
                nb("Change Cell Type (Code / Markdown)", "", N::ToggleKind),
                nb("Join With Previous Cell", "", N::JoinPrevious),
                nb("Join With Next Cell", "", N::JoinNext),
                nb("Copy Cell Down", "", N::CopyDown),
                nb("Move Cell Up", "arrow-up", N::MoveUp),
                nb("Move Cell Down", "arrow-down", N::MoveDown),
                Separator,
                nb("Run Cell", "", N::Run),
                nb("Run Cell and Below", "", N::RunBelow),
                nb("Clear Cell Outputs", "", N::ClearOutputs),
                Separator,
                nb("Delete Cell", "trash", N::Delete),
            ]
        }
        ContextTarget::Output => vec![
            entry("Copy", "copy", ContextAction::Output(OutputAction::Copy), Some((|| Box::new(a::Copy), ed))),
            entry("Select All", "selection-all", ContextAction::Output(OutputAction::SelectAll), Some((|| Box::new(a::SelectAll), ed))),
            Separator,
            entry("Clear Output", "trash", ContextAction::Output(OutputAction::Clear), None),
        ],
        ContextTarget::Manage => {
            use crate::palette::ids;
            let wb = a::WORKBENCH_CONTEXT;
            let command = |label, icon, id, shortcut: Option<ShortcutOf>| entry(label, icon, ContextAction::Command(id), shortcut);
            vec![
                command("Command Palette...", "command", ids::COMMAND_PALETTE, Some((|| Box::new(a::OpenCommandPalette), wb))),
                Separator,
                command("Settings", "gear", ids::OPEN_SETTINGS_PAGE, Some((|| Box::new(a::OpenSettingsPage), wb))),
                command("Keyboard Shortcuts", "keyboard", ids::OPEN_SHORTCUTS_PAGE, Some((|| Box::new(a::OpenShortcutsPage), wb))),
                Separator,
                command("Color Theme...", "palette", ids::SELECT_THEME, None),
                command("File Icon Theme...", "image", ids::SELECT_ICON_THEME, None),
                Separator,
                command("Open Settings (JSON)", "code", ids::OPEN_USER_SETTINGS, None),
                command("Open Keyboard Shortcuts (JSON)", "code", ids::OPEN_KEYBINDINGS, None),
            ]
        }
        ContextTarget::ActivityBar => {
            use crate::palette::ids;
            let command = |label, icon, id| entry(label, icon, ContextAction::Command(id), None);
            vec![
                command("Move Side Bar to the Left", "arrow-left", ids::SIDEBAR_LEFT),
                command("Move Side Bar to the Right", "arrow-right", ids::SIDEBAR_RIGHT),
                command("Move Side Bar to the Top", "arrow-up", ids::SIDEBAR_TOP),
                command("Move Side Bar to the Bottom", "arrow-down", ids::SIDEBAR_BOTTOM),
            ]
        }
        ContextTarget::NoFolder => vec![
            entry("Open Folder...", "folder-open", ContextAction::OpenFolder, Some((|| Box::new(a::OpenFolder), a::WORKBENCH_CONTEXT))),
            entry("New File", "file-plus", ContextAction::NewUntitled, Some((|| Box::new(a::NewFile), a::WORKBENCH_CONTEXT))),
        ],
        ContextTarget::Entry { path, is_dir } => {
            let dir = if *is_dir { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
            let mut items = Vec::new();
            if !*is_dir {
                items.push(entry("Open", "file-text", ContextAction::Open(path.clone()), Some((|| Box::new(a::ExplorerAccept), ex))));
                items.push(Separator);
            }
            items.push(entry("New File...", "file-plus", ContextAction::NewFile(dir.clone()), None));
            items.push(entry("New Folder...", "folder-plus", ContextAction::NewFolder(dir), None));
            items.push(Separator);
            items.push(entry("Cut", "scissors", ContextAction::Cut(path.clone()), Some((|| Box::new(a::ExplorerCut), ex))));
            items.push(entry("Copy", "copy", ContextAction::Copy(path.clone()), Some((|| Box::new(a::ExplorerCopy), ex))));
            if *is_dir {
                items.push(entry("Paste", "clipboard", ContextAction::Paste(path.clone()), Some((|| Box::new(a::ExplorerPaste), ex))));
            }
            items.push(Separator);
            items.push(entry("Rename...", "pencil-simple", ContextAction::Rename(path.clone()), Some((|| Box::new(a::ExplorerRename), ex))));
            items.push(entry("Delete", "trash", ContextAction::Delete(path.clone()), Some((|| Box::new(a::ExplorerDelete), ex))));
            items.push(entry("Copy Path", "link", ContextAction::CopyPath(path.clone()), None));
            items
        }
    }
}

/// How tall the context menu of `target` is, for a menu that opens upwards.
pub(crate) fn context_menu_height(target: &ContextTarget) -> f32 {
    8.0 + context_entries(target).iter().map(|item| if matches!(item, ContextItem::Separator) { 9.0 } else { 26.0 }).sum::<f32>()
}

/// Entries of the context menu for `target`, without the groups.
#[cfg(test)]
pub(crate) fn context_items(target: &ContextTarget) -> Vec<(&'static str, ContextAction)> {
    context_entries(target)
        .into_iter()
        .filter_map(|item| match item {
            ContextItem::Entry(entry) => Some((entry.label, entry.action)),
            ContextItem::Separator => None,
        })
        .collect()
}

/// Marks on the system clipboard text of files copied or cut in the explorer.
const FILES_COPIED: &str = "rustcode:files:copy";
const FILES_CUT: &str = "rustcode:files:cut";

impl EditorView {
    // ---- drawing ---------------------------------------------------------------------------

    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text) = (theme.lists.hover, theme.side_bar.foreground);
        let active_path = self.document.path.as_deref();
        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        let mut row_id = 0usize;
        // Each folder of the window is a project of its own: the explorer shows the active one.
        let shown = self.active_root_index();
        let isolate = self.shell.explorers.len() > 1;
        for (tree_index, tree) in self.shell.explorers.iter().enumerate() {
            if isolate && tree_index != shown {
                continue;
            }
            let header = div()
                .id(("explorer-root", tree_index))
                .h(px(ROW_HEIGHT))
                .flex_none()
                .px(px(8.))
                .flex()
                .items_center()
                .bg(theme.side_bar.section_background)
                .text_color(if theme.side_bar.section_background.a > 0.0 { theme.side_bar.section_foreground } else { text })
                .child(tree.root_name().to_uppercase())
                .drag_over::<DraggedEntries>(move |style, _, _, _| style.bg(hover))
                .on_drop({
                    let root = tree.root().to_path_buf();
                    cx.listener(move |this, dragged: &DraggedEntries, _w, cx| {
                        this.move_paths_into(&dragged.paths, &root, cx);
                    })
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.shell.context_menu = Some(ContextMenu {
                            position: event.position,
                            target: ContextTarget::Root(tree_index),
                        });
                        cx.notify();
                    }),
                );
            sections.push(header.into_any_element());
            for row in tree.rows() {
                row_id += 1;
                sections.push(self.render_row(row_id, row, active_path, cx).into_any_element());
            }
        }

        self.side_panel_frame("sidebar", cx)
            .child(self.side_panel_title("EXPLORER"))
            .child(self.scroll_area(
                1,
                &self.shell.explorer_scroll,
                div()
                    .id("explorer-rows")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.shell.explorer_scroll)
                    // Rows and headers stop the click; what arrives here is the empty part.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            let target = if this.shell.explorers.is_empty() { ContextTarget::NoFolder } else { ContextTarget::Blank };
                            this.shell.context_menu = Some(ContextMenu { position: event.position, target });
                            cx.notify();
                        }),
                    )
                    .children(sections)
                    .children(self.render_ext_views("explorer", cx)),
                cx,
            ))
    }

    /// The column all side bar views share: width, background, and the draggable edge.
    pub(crate) fn side_panel_frame(&self, id: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let hover = theme.lists.hover;
        use crate::shell::SideBarPosition as Position;
        let position = self.shell.side_bar_position;
        let vertical = matches!(position, Position::Left | Position::Right);
        // The draggable edge is the one facing the editor.
        let resize_handle = div()
            .id("sidebar-resize")
            .absolute()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.shell.sidebar_resizing = true;
                    cx.notify();
                }),
            );
        let resize_handle = match position {
            Position::Left => resize_handle.top_0().right(px(-2.)).w(px(5.)).h_full().cursor_col_resize(),
            Position::Right => resize_handle.top_0().left(px(-2.)).w(px(5.)).h_full().cursor_col_resize(),
            Position::Top => resize_handle.left_0().bottom(px(-2.)).h(px(5.)).w_full().cursor_row_resize(),
            Position::Bottom => resize_handle.left_0().top(px(-2.)).h(px(5.)).w_full().cursor_row_resize(),
        };
        div()
            .id(id)
            .relative()
            .flex_none()
            .when(vertical, |frame| frame.w(px(self.shell.sidebar_width)).h_full())
            .when(!vertical, |frame| frame.h(px(self.shell.sidebar_height)).w_full())
            .flex()
            .flex_col()
            .bg(theme.side_bar.background)
            .map(|frame| match position {
                Position::Left => frame.border_r_1(),
                Position::Right => frame.border_l_1(),
                Position::Top => frame.border_b_1(),
                Position::Bottom => frame.border_t_1(),
            })
            .border_color(theme.side_bar.border)
            .text_color(theme.side_bar.foreground)
            .child(resize_handle)
    }

    pub(crate) fn side_panel_title(&self, title: &'static str) -> impl IntoElement {
        div()
            .h(px(28.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_center()
            .text_size(px(11.))
            .text_color(self.theme.side_bar.title_foreground)
            .child(title)
    }

    /// The icon beside a file or folder name: from the icon theme, or a colored dot without one.
    pub(crate) fn entry_icon(&self, name: &str, is_dir: bool, expanded: bool, dim: Rgba) -> gpui::AnyElement {
        let id = if is_dir { self.icons.folder_icon(name, expanded) } else { self.icons.file_icon(name) };
        match id {
            Some(id) => gpui::img(icon_source(id)).w(px(14.)).h(px(14.)).flex_none().into_any_element(),
            None => div()
                .w(px(8.))
                .h(px(8.))
                .flex_none()
                .rounded(px(4.))
                .when(!is_dir, |dot| dot.bg(file_color(name)))
                .when(is_dir, |dot| dot.border_1().border_color(dim))
                .into_any_element(),
        }
    }

    fn render_row(
        &self,
        id: usize,
        row: TreeRow,
        active_path: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.side_bar.foreground, theme.gutter_text);
        let is_active = !row.is_dir && active_path == Some(row.path.as_path());
        let is_cursor = self.shell.focused && self.shell.cursor.as_deref() == Some(row.path.as_path());
        let is_selected = self.shell.selection.contains(&row.path);
        let drag_paths = self.selection_for(&row.path);
        let drag_files: Vec<PathBuf> = {
            let rows = self.shell.all_rows();
            drag_paths
                .iter()
                .filter(|p| rows.iter().any(|(_, r)| &r.path == *p && !r.is_dir))
                .cloned()
                .collect()
        };
        let drag_label = if drag_paths.len() > 1 {
            format!("{} items", drag_paths.len())
        } else {
            row.name.clone()
        };
        let (preview_bg, preview_text) = (theme.widget_background, theme.text);
        let drop_dir = row.is_dir.then(|| row.path.clone());
        let (path, is_dir) = (row.path.clone(), row.is_dir);
        let right_click_path = row.path.clone();
        let chevron = match (row.is_dir, row.expanded) {
            (true, true) => Some("chevron-down"),
            (true, false) => Some("chevron-right"),
            (false, _) => None,
        };
        let decoration = self.git_decoration(&row.path, row.is_dir);
        let mut line = div()
            .id(("explorer-row", id))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .pl(px(8. + 12. * row.depth as f32))
            .pr(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .text_color(match (row.ignored, decoration) {
                (true, _) => theme.git.ignored,
                (false, Some((color, _))) => color,
                (false, None) => text,
            })
            .whitespace_nowrap()
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                    this.explorer_click_with(&path, is_dir, event.modifiers, cx)
                }),
            )
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.shell.cursor = Some(right_click_path.clone());
                    this.shell.context_menu = Some(ContextMenu {
                        position: event.position,
                        target: ContextTarget::Entry { path: right_click_path.clone(), is_dir },
                    });
                    cx.notify();
                }),
            )
            .child(div().w(px(12.)).flex_none().children(chevron.map(|name| crate::icons::icon(name, 12., dim))))
            .child(self.entry_icon(&row.name, row.is_dir, row.expanded, dim))
            .child(row.name)
            .child(div().flex_1())
            .children(decoration.and_then(|(color, letter)| {
                letter.map(|letter| div().flex_none().w(px(14.)).text_color(color).child(letter.to_string()))
            }));
        line = line.on_drag(
            DraggedEntries { paths: drag_paths, files: drag_files },
            move |_dragged, _offset, _w, cx| {
                let label = drag_label.clone();
                cx.new(|_| EntriesPreview { label, background: preview_bg, text: preview_text })
            },
        );
        if let Some(dir) = drop_dir {
            line = line
                .drag_over::<DraggedEntries>(move |style, _, _, _| style.bg(hover))
                .on_drop(cx.listener(move |this, dragged: &DraggedEntries, _w, cx| {
                    this.move_paths_into(&dragged.paths, &dir, cx);
                }));
        }
        // The row with the keyboard is the active selection; others are the inactive kind.
        if is_cursor {
            line = line.bg(theme.lists.active_selection).border_l_2().border_color(theme.focus_border);
            if let Some(color) = theme.lists.active_selection_foreground {
                line = line.text_color(color);
            }
        } else if is_selected || is_active {
            line = line.bg(theme.lists.inactive_selection);
        }
        line
    }

    pub(crate) fn render_context_menu(&self, menu: &ContextMenu, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, panel) = (theme.menu.selection, theme.menu.foreground, theme.menu.background);
        let (dim, separator) = (theme.gutter_text, theme.menu.separator);
        // Entries of extensions' menus have no icons; they keep their own groups.
        let entries: Vec<ContextItem> = match &menu.target {
            ContextTarget::Extension { items, arguments } => {
                let mut list = Vec::new();
                let mut group: Option<&str> = None;
                for item in items {
                    let name = item.group.split('@').next().unwrap_or("");
                    if group.is_some_and(|g| g != name) {
                        list.push(ContextItem::Separator);
                    }
                    group = Some(name);
                    list.push(ContextItem::Entry(ContextEntry {
                        label: Box::leak(item.title.clone().into_boxed_str()),
                        icon: "",
                        action: ContextAction::ExtMenu(item.clone(), arguments.clone()),
                        shortcut: None,
                    }));
                }
                list
            }
            other => context_entries(other),
        };
        let mac = cfg!(target_os = "macos");
        let items = entries.into_iter().enumerate().map(|(index, item)| {
            let entry = match item {
                ContextItem::Separator => {
                    return div().id(("context-separator", index)).h(px(1.)).mx(px(8.)).my(px(4.)).bg(separator).into_any_element();
                }
                ContextItem::Entry(entry) => entry,
            };
            let action = self.resolve_context_action(&menu.target, entry.action.clone());
            let shortcut = entry.shortcut.and_then(|(make, context)| {
                let context = gpui::KeyContext::parse(context).ok()?;
                window
                    .bindings_for_action_in_context(make().as_ref(), context)
                    .first()
                    .map(|b| crate::menu::display_keystrokes(b.keystrokes(), mac))
            });
            div()
                .id(("context-item", index))
                .h(px(26.))
                .mx(px(4.))
                .px(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .rounded(px(3.))
                .text_color(text)
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.shell.context_menu = None;
                        this.run_context_action(action.clone(), cx);
                    }),
                )
                .child(div().w(px(16.)).flex_none().flex().items_center().justify_center().children((!entry.icon.is_empty()).then(|| crate::icons::icon(entry.icon, 14., dim))))
                .child(div().flex_1().whitespace_nowrap().child(entry.label))
                .children(shortcut.map(|keys| div().flex_none().text_size(px(12.)).text_color(dim).child(keys)))
                .into_any_element()
        });
        // Kept inside the window, wherever it was opened.
        let size = window.viewport_size();
        let height = match &menu.target {
            ContextTarget::Extension { .. } => 0.0,
            other => context_menu_height(other),
        };
        let left = menu.position.x.min(size.width - px(CONTEXT_MENU_WIDTH + 4.)).max(px(0.));
        let top = menu.position.y.min(size.height - px(height + 4.)).max(px(0.));
        deferred(
            div()
                .id("explorer-context-menu")
                .absolute()
                .left(left)
                .top(top)
                .w(px(CONTEXT_MENU_WIDTH))
                .py(px(4.))
                .flex()
                .flex_col()
                .rounded(px(6.))
                .border_1()
                .border_color(hover)
                .bg(panel)
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    this.shell.context_menu = None;
                    cx.notify();
                }))
                .children(items)
                .child(self.overlay_probe()),
        )
        .with_priority(20)
    }

    /// Fill in what the generic entry list leaves open (the folder of a root header).
    fn resolve_context_action(&self, target: &ContextTarget, action: ContextAction) -> ContextAction {
        let root_dir = |index: usize| self.shell.explorers.get(index).map(|t| t.root().to_path_buf());
        match (target, action) {
            (ContextTarget::Root(i), ContextAction::NewFile(_)) => {
                ContextAction::NewFile(root_dir(*i).unwrap_or_default())
            }
            (ContextTarget::Root(i), ContextAction::NewFolder(_)) => {
                ContextAction::NewFolder(root_dir(*i).unwrap_or_default())
            }
            (ContextTarget::Root(i), ContextAction::Paste(_)) => {
                ContextAction::Paste(root_dir(*i).unwrap_or_default())
            }
            // The empty part of the explorer acts on the first folder.
            (ContextTarget::Blank, ContextAction::NewFile(_)) => ContextAction::NewFile(root_dir(0).unwrap_or_default()),
            (ContextTarget::Blank, ContextAction::NewFolder(_)) => ContextAction::NewFolder(root_dir(0).unwrap_or_default()),
            (ContextTarget::Blank, ContextAction::Paste(_)) => ContextAction::Paste(root_dir(0).unwrap_or_default()),
            (_, other) => other,
        }
    }

    pub(crate) fn run_context_action(&mut self, action: ContextAction, cx: &mut Context<Self>) {
        match action {
            ContextAction::Open(path) => self.open_path(&path, cx),
            ContextAction::NewFile(dir) => self.open_prompt(PaletteMode::NewFile, "", Some(dir), cx),
            ContextAction::NewFolder(dir) => self.open_prompt(PaletteMode::NewFolder, "", Some(dir), cx),
            ContextAction::Rename(path) => self.begin_rename(&path, cx),
            ContextAction::Delete(path) => {
                // A right-click inside a multi-selection acts on all of it.
                let paths = self.selection_for(&path);
                self.delete_paths(&paths, cx)
            }
            ContextAction::Cut(path) => {
                let paths = self.selection_for(&path);
                self.copy_to_file_clipboard(paths, true, cx)
            }
            ContextAction::Copy(path) => {
                let paths = self.selection_for(&path);
                self.copy_to_file_clipboard(paths, false, cx)
            }
            ContextAction::Paste(dir) => self.paste_files(&dir, cx),
            ContextAction::CopyPath(path) => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                self.set_status(format!("copied {}", path.display()));
                cx.notify();
            }
            ContextAction::RemoveRoot(index) => self.remove_workspace_folder(index, cx),
            ContextAction::Refresh => self.refresh_explorer(cx),
            ContextAction::OpenFolder => self.open_folder_dialog(cx),
            ContextAction::NewUntitled => self.new_file(cx),
            ContextAction::Editor(command) => self.run_editor_command(command, cx),
            ContextAction::Notebook(command) => self.run_notebook_command(command, cx),
            ContextAction::Command(id) => self.run_command(id, cx),
            ContextAction::Output(OutputAction::Copy) => self.copy_output(cx),
            ContextAction::Output(OutputAction::SelectAll) => self.select_all_output(cx),
            ContextAction::Output(OutputAction::Clear) => self.clear_output(cx),
            ContextAction::ExtMenu(item, arguments) => self.run_menu_command_public(&item, arguments, cx),
        }
    }

    /// An entry of the editor's right-click menu: the same code the shortcuts reach.
    fn run_editor_command(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        use EditorCommand as C;
        match command {
            C::GoToDefinition => self.go_to_definition(cx),
            C::FindReferences => self.find_references(cx),
            C::Rename => self.start_rename(cx),
            C::QuickFix => self.show_code_actions(cx),
            C::Format => {
                self.format_document(false, cx);
            }
            C::ToggleComment => self.toggle_line_comment(cx),
            C::Cut => self.cut(cx),
            C::Copy => self.copy(cx),
            C::Paste => self.paste(cx),
            C::SelectAll => self.edit_document(cx, |e| e.select_all()),
            C::CommandPalette => self.open_palette(PaletteMode::Commands, cx),
        }
    }

    // ---- mouse -----------------------------------------------------------------------------

    /// A plain click: select the row, then open the file or toggle the folder.
    pub(crate) fn explorer_click(&mut self, path: &Path, is_dir: bool, cx: &mut Context<Self>) {
        self.explorer_click_with(path, is_dir, Modifiers::none(), cx)
    }

    /// Click with modifiers: the platform's command key toggles a row in the selection, shift
    /// selects a range.
    pub(crate) fn explorer_click_with(
        &mut self,
        path: &Path,
        is_dir: bool,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.shell.context_menu = None;
        self.shell.delete_armed.clear();
        self.shell.focused = true;
        if modifiers.secondary() {
            if !self.shell.selection.remove(path) {
                self.shell.selection.insert(path.to_path_buf());
            }
            self.shell.cursor = Some(path.to_path_buf());
            self.shell.range_anchor = Some(path.to_path_buf());
            cx.notify();
            return;
        }
        if modifiers.shift {
            let anchor = self.shell.range_anchor.clone().or_else(|| self.shell.cursor.clone());
            self.select_range(anchor.as_deref(), path);
            self.shell.cursor = Some(path.to_path_buf());
            cx.notify();
            return;
        }
        self.shell.selection.clear();
        self.shell.selection.insert(path.to_path_buf());
        self.shell.range_anchor = Some(path.to_path_buf());
        self.shell.cursor = Some(path.to_path_buf());
        if is_dir {
            self.toggle_folder(path);
            cx.notify();
        } else {
            self.open_path(path, cx);
            // Opening a file hands the keyboard to the editor.
            self.shell.focused = false;
        }
    }

    /// Select every visible row from `anchor` to `to`, replacing the selection.
    fn select_range(&mut self, anchor: Option<&Path>, to: &Path) {
        let rows = self.shell.all_rows();
        let position = |p: &Path| rows.iter().position(|(_, r)| r.path == p);
        let (Some(a), Some(b)) = (anchor.and_then(position), position(to)) else {
            self.shell.selection = std::iter::once(to.to_path_buf()).collect();
            return;
        };
        let (from, until) = (a.min(b), a.max(b));
        self.shell.selection = rows[from..=until].iter().map(|(_, r)| r.path.clone()).collect();
    }

    /// Rows an action on `path` applies to: the whole selection when `path` is in it.
    pub(crate) fn selection_for(&self, path: &Path) -> Vec<PathBuf> {
        if self.shell.selection.contains(path) {
            self.shell
                .all_rows()
                .into_iter()
                .map(|(_, r)| r.path)
                .filter(|p| self.shell.selection.contains(p))
                .collect()
        } else {
            vec![path.to_path_buf()]
        }
    }

    /// Shift+Up / Shift+Down: grow the selection from where it started.
    pub(crate) fn explorer_extend(&mut self, delta: isize, cx: &mut Context<Self>) {
        let anchor = self.shell.range_anchor.clone().or_else(|| self.shell.cursor.clone());
        self.explorer_move(delta, cx);
        if let Some(cursor) = self.shell.cursor.clone() {
            self.shell.range_anchor = anchor.clone();
            self.select_range(anchor.as_deref(), &cursor);
        }
        cx.notify();
    }

    fn toggle_folder(&mut self, path: &Path) {
        if let Some(index) = self.shell.explorer_for(path) {
            self.shell.explorers[index].toggle(path);
            self.shell.sync_watches();
        }
    }

    /// How wide the side bar's content is: the bar's width, or the window's when it lies along the top or bottom.
    pub(crate) fn side_panel_width(&self, window: &Window) -> f32 {
        if self.shell.side_bar_position.is_vertical() {
            self.shell.sidebar_width
        } else {
            window.viewport_size().width.into()
        }
    }

    /// Drag of the side bar's edge.
    pub(crate) fn drag_sidebar(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        use crate::shell::SideBarPosition as Position;
        if event.pressed_button != Some(MouseButton::Left) {
            self.shell.sidebar_resizing = false;
            return;
        }
        let (x, y): (f32, f32) = (event.position.x.into(), event.position.y.into());
        let size = window.viewport_size();
        let (window_width, window_height): (f32, f32) = (size.width.into(), size.height.into());
        let bar = crate::shell::ACTIVITY_BAR_WIDTH;
        match self.shell.side_bar_position {
            Position::Left => self.shell.sidebar_width = (x - bar).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
            Position::Right => self.shell.sidebar_width = (window_width - x - bar).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
            Position::Top => {
                let banner = if self.shell.trust_banner { crate::problems::TRUST_BANNER_HEIGHT } else { 0.0 };
                let top = self.title_bar_height(window) + banner + bar;
                self.shell.sidebar_height = (y - top).clamp(crate::shell::MIN_SIDEBAR_HEIGHT, (window_height * 0.6).max(crate::shell::MIN_SIDEBAR_HEIGHT));
            }
            Position::Bottom => {
                let bottom = window_height - crate::workbench::STATUS_BAR_HEIGHT - bar;
                self.shell.sidebar_height = (bottom - y).clamp(crate::shell::MIN_SIDEBAR_HEIGHT, (window_height * 0.6).max(crate::shell::MIN_SIDEBAR_HEIGHT));
            }
        }
        cx.notify();
    }

    /// Put the side bar (and the activity bar) along `position`, and keep the choice in the user settings.
    pub(crate) fn move_sidebar(&mut self, position: crate::shell::SideBarPosition, cx: &mut Context<Self>) {
        self.shell.sidebar_visible = true;
        self.set_scope_setting(
            crate::app_pages::Scope::User,
            "workbench.sideBar.location",
            (position != crate::shell::SideBarPosition::Left).then(|| serde_json::json!(position.name())),
            cx,
        );
        // Without a settings file to save to, the move still holds for this session.
        self.shell.side_bar_position = position;
        cx.notify();
    }

    /// Show or hide the minimap, and keep the choice in the user settings.
    pub(crate) fn toggle_minimap(&mut self, cx: &mut Context<Self>) {
        let on = !self.minimap_on();
        self.set_scope_setting(crate::app_pages::Scope::User, "editor.minimap.enabled", (!on).then(|| serde_json::json!(false)), cx);
        self.config.minimap = on;
        cx.notify();
    }

    // ---- keyboard --------------------------------------------------------------------------

    pub(crate) fn focus_explorer(&mut self, cx: &mut Context<Self>) {
        self.shell.sidebar_visible = true;
        self.shell.focused = true;
        self.reveal_active_file();
        let rows = self.shell.all_rows();
        let keep = self.shell.cursor.as_ref().filter(|c| rows.iter().any(|(_, r)| &r.path == *c)).cloned();
        self.shell.cursor = keep
            .or_else(|| self.document.path.clone().filter(|p| rows.iter().any(|(_, r)| &r.path == p)))
            .or_else(|| rows.first().map(|(_, r)| r.path.clone()));
        cx.notify();
    }

    pub(crate) fn explorer_back(&mut self, cx: &mut Context<Self>) {
        if self.shell.context_menu.take().is_none() {
            self.shell.focused = false;
        }
        cx.notify();
    }

    pub(crate) fn cursor_row(&self) -> Option<(usize, TreeRow)> {
        let cursor = self.shell.cursor.as_ref()?;
        self.shell.all_rows().into_iter().find(|(_, row)| &row.path == cursor)
    }

    pub(crate) fn explorer_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.shell.all_rows();
        if rows.is_empty() {
            return;
        }
        let current = self
            .shell
            .cursor
            .as_ref()
            .and_then(|c| rows.iter().position(|(_, r)| &r.path == c));
        let next = match current {
            Some(i) => (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.shell.cursor = Some(rows[next].1.path.clone());
        self.shell.delete_armed.clear();
        let target = rows[next].1.path.clone();
        self.scroll_explorer_to(&target);
        cx.notify();
    }

    /// Keep the highlighted row in view. Each folder header counts as a child of the list.
    fn scroll_explorer_to(&self, path: &Path) {
        let mut index = 0;
        for tree in &self.shell.explorers {
            index += 1;
            for row in tree.rows() {
                if row.path == path {
                    self.shell.explorer_scroll.scroll_to_item(index);
                    return;
                }
                index += 1;
            }
        }
    }

    /// Left: collapse an open folder, else go to the parent folder.
    pub(crate) fn explorer_left(&mut self, cx: &mut Context<Self>) {
        let Some((_, row)) = self.cursor_row() else { return };
        if row.is_dir && row.expanded {
            self.toggle_folder(&row.path);
        } else if let Some(parent) = row.path.parent() {
            let rows = self.shell.all_rows();
            if rows.iter().any(|(_, r)| r.path == parent) {
                self.shell.cursor = Some(parent.to_path_buf());
            }
        }
        cx.notify();
    }

    /// Right: open a closed folder, else step into it.
    pub(crate) fn explorer_right(&mut self, cx: &mut Context<Self>) {
        let Some((_, row)) = self.cursor_row() else { return };
        if row.is_dir && !row.expanded {
            self.toggle_folder(&row.path);
        } else if row.is_dir {
            self.explorer_move(1, cx);
            return;
        }
        cx.notify();
    }

    /// Enter: open the file, or toggle the folder.
    pub(crate) fn explorer_accept(&mut self, cx: &mut Context<Self>) {
        let Some((_, row)) = self.cursor_row() else { return };
        self.explorer_click(&row.path, row.is_dir, cx);
        if row.is_dir {
            self.shell.focused = true;
        }
    }

    pub(crate) fn explorer_rename(&mut self, cx: &mut Context<Self>) {
        if let Some((_, row)) = self.cursor_row() {
            self.begin_rename(&row.path, cx);
        }
    }

    pub(crate) fn explorer_delete(&mut self, cx: &mut Context<Self>) {
        if let Some((_, row)) = self.cursor_row() {
            self.delete_path(&row.path, cx);
        }
    }

    // ---- file operations -------------------------------------------------------------------

    fn begin_rename(&mut self, path: &Path, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.open_prompt(PaletteMode::Rename, &name, Some(path.to_path_buf()), cx);
    }

    /// Refresh the explorers and make `path` visible and highlighted.
    pub(crate) fn show_in_explorer(&mut self, path: &Path) {
        for tree in &mut self.shell.explorers {
            tree.refresh();
        }
        if let Some(index) = self.shell.explorer_for(path) {
            self.shell.explorers[index].reveal(path);
        }
        self.shell.sync_watches();
        self.shell.cursor = Some(path.to_path_buf());
        self.shell.selection = std::iter::once(path.to_path_buf()).collect();
    }

    pub(crate) fn create_file_at(&mut self, path: &Path, cx: &mut Context<Self>) {
        match self.file_ops.create_file(path) {
            Ok(()) => {
                self.log(format!("created {}", path.display()));
                self.show_in_explorer(path);
                self.open_path(path, cx);
            }
            Err(err) => self.set_status(err.to_string()),
        }
        cx.notify();
    }

    pub(crate) fn create_folder_at(&mut self, path: &Path, cx: &mut Context<Self>) {
        match self.file_ops.create_dir(path) {
            Ok(()) => {
                self.log(format!("created {}", path.display()));
                self.show_in_explorer(path);
                if let Some(index) = self.shell.explorer_for(path) {
                    if !self.shell.explorers[index].is_expanded(path) {
                        self.shell.explorers[index].toggle(path);
                        self.shell.sync_watches();
                    }
                }
            }
            Err(err) => self.set_status(err.to_string()),
        }
        cx.notify();
    }

    pub(crate) fn rename_path(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        if from == to {
            return;
        }
        match self.file_ops.rename(from, to) {
            Ok(()) => {
                self.log(format!("renamed {} to {}", from.display(), to.display()));
                self.retarget_documents(from, to);
                self.show_in_explorer(to);
            }
            Err(err) => self.set_status(err.to_string()),
        }
        cx.notify();
    }

    /// Open documents inside a renamed file or folder follow it.
    pub(crate) fn retarget_documents(&mut self, from: &Path, to: &Path) {
        let ids: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).collect();
        for id in ids {
            let new_path = self.doc_path(id).and_then(|path| {
                path.strip_prefix(from).ok().map(|rest| {
                    if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) }
                })
            });
            let Some(new_path) = new_path else { continue };
            let title = new_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| new_path.display().to_string());
            let highlighter = self.make_highlighter(&title);
            let document = if id == self.document.id {
                Some((&mut self.document, &mut self.highlighter))
            } else {
                self.docs
                    .iter_mut()
                    .find(|slot| slot.id == id)
                    .and_then(|slot| slot.parked.as_mut())
                    .map(|parked| (&mut parked.document, &mut parked.highlighter))
            };
            if let Some((document, slot_highlighter)) = document {
                document.path = Some(new_path);
                document.title = title;
                *slot_highlighter = highlighter;
            }
        }
    }

    /// Move to the trash. The first request only arms the delete.
    pub(crate) fn delete_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.delete_paths(&[path.to_path_buf()], cx);
    }

    pub(crate) fn delete_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let what = match paths {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        if self.shell.delete_armed != paths {
            self.shell.delete_armed = paths.to_vec();
            self.set_status(format!("delete {what}? request Delete again to move to the trash"));
            cx.notify();
            return;
        }
        self.shell.delete_armed.clear();
        let rows = self.shell.all_rows();
        let neighbour = rows
            .iter()
            .position(|(_, r)| paths.contains(&r.path))
            .and_then(|first| {
                rows.iter()
                    .skip(first)
                    .find(|(_, r)| !paths.iter().any(|p| r.path.starts_with(p)))
                    .or_else(|| rows[..first].iter().rev().find(|(_, r)| !paths.iter().any(|p| r.path.starts_with(p))))
            })
            .map(|(_, r)| r.path.clone());
        let mut failed = None;
        for path in paths {
            match self.file_ops.trash(path) {
                Ok(()) => self.log(format!("moved {} to the trash", path.display())),
                Err(err) => failed = Some(err.to_string()),
            }
        }
        for tree in &mut self.shell.explorers {
            tree.refresh();
        }
        self.shell.sync_watches();
        self.shell.selection.clear();
        self.shell.cursor = neighbour;
        self.set_status(failed.unwrap_or_else(|| format!("moved {what} to the trash")));
        cx.notify();
    }

    // ---- copy, cut, paste, move ------------------------------------------------------------

    pub(crate) fn copy_to_file_clipboard(&mut self, paths: Vec<PathBuf>, cut: bool, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let verb = if cut { "cut" } else { "copied" };
        self.set_status(format!("{verb} {} item{}", paths.len(), if paths.len() == 1 { "" } else { "s" }));
        // The paths go on the system clipboard, one per line, so another window (or a text
        // field or terminal) gets them too. The mark says whether it was a cut.
        let text = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
        let mark = if cut { FILES_CUT } else { FILES_COPIED };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string_with_metadata(text, mark.to_owned()));
        self.shell.file_clipboard = Some(FileClipboard { paths, cut });
        cx.notify();
    }

    /// The files on the system clipboard: absolute paths that exist, one per line. They may have
    /// been copied in this window, in another one, or typed as text anywhere.
    fn clipboard_files(&self, cx: &Context<Self>) -> Option<FileClipboard> {
        let item = cx.read_from_clipboard()?;
        let text = item.text()?;
        let paths: Vec<PathBuf> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        let exists = |path: &PathBuf| {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return false };
            path.is_absolute()
                && self.shell.reader.list_dir(parent).is_ok_and(|entries| entries.iter().any(|e| e.name.as_str() == name.to_string_lossy()))
        };
        if paths.is_empty() || !paths.iter().all(exists) {
            return None;
        }
        Some(FileClipboard { paths, cut: item.metadata().is_some_and(|m| m == FILES_CUT) })
    }

    pub(crate) fn explorer_copy(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(cursor) = self.shell.cursor.clone() else { return };
        let paths = self.selection_for(&cursor);
        self.copy_to_file_clipboard(paths, cut, cx);
    }

    /// Folder a paste goes to: the highlighted folder, or the folder of the highlighted file.
    fn paste_target(&self) -> Option<PathBuf> {
        match self.cursor_row() {
            Some((_, row)) if row.is_dir => Some(row.path),
            Some((_, row)) => row.path.parent().map(Path::to_path_buf),
            None => self.shell.explorers.first().map(|t| t.root().to_path_buf()),
        }
    }

    pub(crate) fn explorer_paste(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.paste_target() {
            self.paste_files(&dir, cx);
        }
    }

    /// Paste what was copied or cut into `dir`. Name clashes get a " copy" suffix.
    pub(crate) fn paste_files(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Some(clipboard) = self.clipboard_files(cx) else {
            self.set_status("nothing to paste");
            cx.notify();
            return;
        };
        let mut taken: HashSet<String> = self
            .shell
            .reader
            .list_dir(dir)
            .map(|entries| entries.into_iter().map(|e| e.name).collect())
            .unwrap_or_default();
        let mut last = None;
        let mut error = None;
        for from in &clipboard.paths {
            let Some(name) = from.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            // Cutting into the folder it is already in changes nothing.
            if clipboard.cut && from.parent() == Some(dir) {
                continue;
            }
            if dir.starts_with(from) {
                error = Some(format!("cannot paste {name} into itself"));
                continue;
            }
            let new_name = copy_name(&name, &taken);
            let to = dir.join(&new_name);
            let result = if clipboard.cut {
                self.file_ops.rename(from, &to)
            } else {
                self.file_ops.copy(from, &to)
            };
            match result {
                Ok(()) => {
                    taken.insert(new_name);
                    if clipboard.cut {
                        self.retarget_documents(from, &to);
                    }
                    self.log(format!(
                        "{} {} to {}",
                        if clipboard.cut { "moved" } else { "copied" },
                        from.display(),
                        to.display()
                    ));
                    last = Some(to);
                }
                Err(err) => error = Some(err.to_string()),
            }
        }
        if clipboard.cut && error.is_none() {
            // A cut is used up: the files are no longer where the clipboard says.
            self.shell.file_clipboard = None;
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
        }
        if let Some(path) = last {
            self.show_in_explorer(&path);
        }
        if let Some(message) = error {
            self.set_status(message);
        }
        cx.notify();
    }

    /// Drag and drop: move `paths` into folder `dir`.
    pub(crate) fn move_paths_into(&mut self, paths: &[PathBuf], dir: &Path, cx: &mut Context<Self>) {
        let mut moved = None;
        let mut error = None;
        for from in paths {
            let Some(name) = from.file_name() else { continue };
            if from.parent() == Some(dir) {
                continue;
            }
            if dir.starts_with(from) {
                error = Some(format!("cannot move {} into itself", name.to_string_lossy()));
                continue;
            }
            let to = dir.join(name);
            match self.file_ops.rename(from, &to) {
                Ok(()) => {
                    self.log(format!("moved {} to {}", from.display(), to.display()));
                    self.retarget_documents(from, &to);
                    moved = Some(to);
                }
                Err(err) => error = Some(err.to_string()),
            }
        }
        if let Some(path) = moved {
            self.shell.selection.clear();
            self.show_in_explorer(&path);
            // Show the destination so the move is visible.
            if let Some(index) = self.shell.explorer_for(dir) {
                if !self.shell.explorers[index].is_expanded(dir) {
                    self.shell.explorers[index].toggle(dir);
                    self.shell.sync_watches();
                }
            }
        }
        if let Some(message) = error {
            self.set_status(message);
        }
        cx.notify();
    }

    // ---- workspace folders -----------------------------------------------------------------

    pub(crate) fn add_workspace_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.shell.explorers.iter().any(|t| t.root() == path) {
            self.set_status(format!("{} is already in the workspace", path.display()));
        } else if self.shell.reader.list_dir(&path).is_err() {
            self.set_status(format!("{} is not a folder", path.display()));
        } else {
            self.shell
                .explorers
                .push(rustcode_application::ExplorerTree::new(path.clone(), self.shell.reader.clone()));
            self.shell.sync_watches();
            self.shell.sidebar_visible = true;
            self.log(format!("added {} to the workspace", path.display()));
            self.warn_if_untrusted(cx);
        }
        cx.notify();
    }

    /// Make `path` the workspace: the explorer shows only it.
    /// "Open Folder": the system's folder chooser. Where there is none to show (tests), the
    /// palette asks for the path instead.
    pub(crate) fn open_folder_dialog(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) {
            return self.open_prompt(PaletteMode::OpenFolder, "~/", None, cx);
        }
        self.close_palette(true, cx);
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            match chosen.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.update(cx, |this, cx| this.open_project(&path, cx)).ok();
                    }
                }
                // The chooser could not be shown (some Linux setups): ask for the path in the palette.
                Ok(Err(_)) => {
                    this.update(cx, |this, cx| this.open_prompt(PaletteMode::OpenFolder, "~/", None, cx)).ok();
                }
                _ => {}
            }
        })
        .detach();
    }

    /// Open a project: in this window when it has no folder yet, else in a window of its own, so
    /// the project that is open stays as it is.
    pub(crate) fn open_project(&mut self, path: &Path, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.shell.explorers.is_empty() || self.shell.reader.list_dir(&path).is_err() {
            return self.open_workspace_folder(&path, cx);
        }
        if self.shell.explorers.iter().any(|tree| tree.root() == path) {
            return self.switch_to_root(self.shell.explorers.iter().position(|t| t.root() == path).unwrap_or(0), cx);
        }
        // This window has a project already: ask where the new one goes.
        self.open_choice = Some(path);
        cx.notify();
    }

    /// Open `path` in a window of its own.
    pub(crate) fn open_in_new_window(&mut self, path: &Path, cx: &mut Context<Self>) {
        match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg(path).spawn()) {
            Ok(_) => self.set_status(format!("opened {} in a new window", path.display())),
            Err(error) => self.set_status(format!("cannot open a new window: {error}")),
        }
        cx.notify();
    }

    pub(crate) fn open_workspace_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.shell.reader.list_dir(&path).is_err() {
            self.set_status(format!("{} is not a folder", path.display()));
        } else {
            self.shell.explorers =
                vec![rustcode_application::ExplorerTree::new(path.clone(), self.shell.reader.clone())];
            self.shell.sync_watches();
            self.shell.sidebar_visible = true;
            self.shell.side_view = crate::shell::SideView::Explorer;
            self.git_state = crate::git_view::GitState::new();
            self.refresh_git(cx);
            self.log(format!("opened folder {}", path.display()));
            self.warn_if_untrusted(cx);
        }
        cx.notify();
    }

    pub(crate) fn remove_workspace_folder(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.shell.explorers.len() < 2 {
            self.set_status("a workspace needs at least one folder");
        } else if index < self.shell.explorers.len() {
            let removed = self.shell.explorers.remove(index);
            self.switched_root = None;
            self.shell.sync_watches();
            self.log(format!("removed {} from the workspace", removed.root().display()));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod manage_menu_tests {
    use super::*;

    #[test]
    fn the_gear_menu_lists_settings_and_shortcuts_with_icons() {
        let entries: Vec<_> = context_entries(&ContextTarget::Manage)
            .into_iter()
            .filter_map(|item| match item {
                ContextItem::Entry(entry) => Some((entry.label, entry.icon, entry.shortcut.is_some())),
                ContextItem::Separator => None,
            })
            .collect();
        assert!(entries.contains(&("Settings", "gear", true)));
        assert!(entries.contains(&("Keyboard Shortcuts", "keyboard", true)));
        assert!(entries.iter().all(|(_, icon, _)| !icon.is_empty()));
        assert!(context_menu_height(&ContextTarget::Manage) > 100.0);
    }

    #[test]
    fn commands_get_the_icon_of_their_own_or_of_their_category() {
        use crate::palette::{command_icon, ids};
        assert_eq!(command_icon(ids::OPEN_SETTINGS_PAGE, "Preferences: Open Settings"), "gear");
        assert_eq!(command_icon(ids::OPEN_SHORTCUTS_PAGE, "x"), "keyboard");
        assert_eq!(command_icon(ids::GIT_PUSH, "Git: Push"), "git");
        assert_eq!(command_icon(ids::DEBUG_STOP, "Debug: Stop"), "bug");
    }
}

//! The path of the open file above the editor, one clickable part per folder.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent};

use crate::editor_view::EditorView;
use crate::shell::SideView;

pub(crate) const BREADCRUMB_HEIGHT: f32 = 22.0;

/// The parts of `path` to show: the workspace folder's name, the folders below it, the file. A
/// file outside every workspace folder shows its last three parts. Each part carries the path it
/// stands for.
pub(crate) fn path_parts(path: &Path, roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let name = |p: &Path| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
    // The deepest folder that holds the file.
    let root = roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.components().count());
    let (start, base): (usize, PathBuf) = match root {
        Some(root) => (0, root.clone()),
        None => {
            let parts: Vec<_> = path.ancestors().collect();
            // `ancestors` lists the file first; keep up to three parts.
            let keep = parts.len().saturating_sub(1).min(3);
            let top = parts.get(keep).map_or_else(PathBuf::new, |p| (*p).to_path_buf());
            (1, top)
        }
    };
    let mut parts = Vec::new();
    if start == 0 {
        parts.push((name(&base), base.clone()));
    }
    let relative = path.strip_prefix(&base).unwrap_or(path);
    let mut current = base;
    for component in relative.components() {
        current.push(component);
        parts.push((component.as_os_str().to_string_lossy().into_owned(), current.clone()));
    }
    parts
}

impl EditorView {
    pub(crate) fn breadcrumbs_enabled(&self) -> bool {
        self.config.editor.breadcrumbs.unwrap_or(true)
    }

    /// Show a part of the path in the explorer.
    pub(crate) fn reveal_breadcrumb(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.show_side_view(SideView::Explorer, cx);
        self.show_in_explorer(path);
        cx.notify();
    }

    pub(crate) fn render_breadcrumbs(&self, gi: usize, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.breadcrumbs_enabled() {
            return None;
        }
        let group = &self.groups[gi];
        let path = self.doc_path(group.tabs[group.active].doc)?.to_path_buf();
        let roots: Vec<PathBuf> = self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
        let parts = path_parts(&path, &roots);
        let theme = &self.theme;
        let colors = &theme.breadcrumbs;
        let hover = theme.lists.hover;
        // Then the names of the code around the caret, when a server has told what they are.
        let symbols = self.symbols_at_caret();
        let file_parts = parts.len();
        let last = file_parts.saturating_sub(1) + symbols.len();
        let mut row = div()
            .id(("breadcrumbs", gi))
            .h(px(BREADCRUMB_HEIGHT))
            .flex_none()
            .px(px(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(colors.background)
            .text_size(px(12.));
        let symbol_parts: Vec<(String, PathBuf, Option<(usize, usize)>)> =
            symbols.iter().map(|s| (s.name.clone(), s.path.clone(), Some((s.line, s.column)))).collect();
        let all_parts = parts.into_iter().map(|(l, p)| (l, p, None)).chain(symbol_parts);
        for (index, (label, target, place)) in all_parts.enumerate() {
            if index > 0 {
                row = row.child(crate::icons::icon("chevron-right", 10., colors.foreground));
            }
            row = row.child(
                div()
                    .id(("breadcrumb", gi * 1000 + index))
                    .px(px(3.))
                    .rounded(px(3.))
                    .text_color(if index == last { colors.focus_foreground } else { colors.foreground })
                    .hover(move |style| style.bg(hover))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| match place {
                            // A name goes to where it is defined; a folder or file shows in the explorer.
                            Some((line, column)) => this.open_path_at(&target, line, column, 0, cx),
                            None => this.reveal_breadcrumb(&target, cx),
                        }),
                    )
                    .child(label),
            );
        }
        Some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(parts: &[(String, PathBuf)]) -> Vec<&str> {
        parts.iter().map(|(l, _)| l.as_str()).collect()
    }

    #[test]
    fn a_file_in_the_workspace_shows_the_folder_then_its_path() {
        let roots = [PathBuf::from("/work/demo")];
        let parts = path_parts(Path::new("/work/demo/src/util/mod.rs"), &roots);
        assert_eq!(labels(&parts), ["demo", "src", "util", "mod.rs"]);
        assert_eq!(parts[0].1, PathBuf::from("/work/demo"));
        assert_eq!(parts[2].1, PathBuf::from("/work/demo/src/util"));
        assert_eq!(parts[3].1, PathBuf::from("/work/demo/src/util/mod.rs"));
    }

    #[test]
    fn the_deepest_workspace_folder_is_the_start() {
        let roots = [PathBuf::from("/work"), PathBuf::from("/work/demo")];
        assert_eq!(labels(&path_parts(Path::new("/work/demo/a.rs"), &roots)), ["demo", "a.rs"]);
    }

    #[test]
    fn a_file_outside_the_workspace_shows_its_last_parts() {
        let parts = path_parts(Path::new("/etc/nginx/conf.d/site.conf"), &[PathBuf::from("/work")]);
        assert_eq!(labels(&parts), ["nginx", "conf.d", "site.conf"]);
        assert_eq!(parts[2].1, PathBuf::from("/etc/nginx/conf.d/site.conf"));
        assert_eq!(labels(&path_parts(Path::new("/a.txt"), &[])), ["a.txt"]);
    }

    #[test]
    fn a_file_directly_in_the_workspace_folder() {
        let parts = path_parts(Path::new("/work/a.txt"), &[PathBuf::from("/work")]);
        assert_eq!(labels(&parts), ["work", "a.txt"]);
    }
}
